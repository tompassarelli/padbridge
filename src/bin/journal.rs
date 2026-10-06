#![deny(unsafe_code)]

#[cfg(target_os = "linux")]
#[path = "../focus.rs"]
mod focus;
#[cfg(target_os = "linux")]
#[path = "../wlr.rs"]
mod wlr;
#[cfg(target_os = "linux")]
#[path = "../stick.rs"]
mod stick;

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("wc3-journal requires Linux evdev event timestamps");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
mod linux {
    #![allow(unsafe_code)]

    use crate::focus::Foreground;
    use crate::stick::{melee_stick, stick_down};
    use enigo::{Direction, Enigo, Key as OutputKey, Keyboard, Settings};
    use evdev::{AbsoluteAxisCode as Abs, EventSummary, KeyCode as Key, raw_stream::RawDevice};
    use std::{
        collections::{BTreeMap, BTreeSet, VecDeque},
        env,
        fs::{self, OpenOptions},
        io::{self, Read, Write},
        os::{fd::AsRawFd, unix::fs::MetadataExt},
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread,
        time::{Duration, Instant, UNIX_EPOCH},
    };

    const HZ: u128 = 60;
    const LAST_FRAME: u32 = 2_147_483_646;
    const ALPHABET: &[u8; 64] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz-_";
    const MOVE_LEFT: u32 = 1 << 0;
    const MOVE_RIGHT: u32 = 1 << 1;
    const MOVE_DOWN: u32 = 1 << 2;
    const MOVE_UP: u32 = 1 << 3;
    const JUMP: u32 = 1 << 4;
    const ATTACK: u32 = 1 << 5;
    const SPECIAL: u32 = 1 << 6;
    const GRAB: u32 = 1 << 7;
    const LEFT_TRIGGER: u32 = 1 << 8;
    const RIGHT_TRIGGER: u32 = 1 << 9;
    const SMASH_LEFT: u32 = 1 << 10;
    const SMASH_RIGHT: u32 = 1 << 11;
    const SMASH_UP: u32 = 1 << 12;
    const SMASH_DOWN: u32 = 1 << 13;
    const WALK: u32 = 1 << 14;
    const EVIOCSCLOCKID: libc::c_ulong = 0x4004_45a0;

    #[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
    struct State {
        sources: u32,
        x: i16,
        y: i16,
        cx: i16,
        cy: i16,
        lt: u16,
        rt: u16,
    }

    #[derive(Default, Clone, Copy)]
    struct Edges {
        pressed: u32,
        released: u32,
        special_x: i16,
        special_y: i16,
        dodge_x: i16,
        dodge_y: i16,
        sdi_x: i16,
        sdi_y: i16,
        ledge: i16,
        throw_x: i16,
        throw_y: i16,
    }

    struct Options {
        device: PathBuf,
        out: PathBuf,
        build: String,
        epoch: u32,
        slot: u32,
        delay: u32,
        epoch_ns: u128,
        first_frame: u32,
        stop_frame: Option<u32>,
        ready_file: Option<PathBuf>,
        mailbox_display: Option<String>,
        editbox_display: Option<String>,
        text_out: Option<PathBuf>,
        trace: bool,
        follow_matches: bool,
        window: Option<u32>,
        pid: Option<u32>,
        niri_window: Option<u64>,
        wlr_app_id: Option<String>,
    }

    // Bound the encoded send queue independently of the timestamped capture
    // queue. Native receive/consume credit may lag capture without losing rows.
    const OUTPUT_RECORD_LIMIT: usize = 120;
    const OUTPUT_BYTE_LIMIT: usize = 2048;

    #[derive(Default)]
    struct PendingOutput {
        records: VecDeque<String>,
        bytes: usize,
    }

    impl PendingOutput {
        fn frame_credit(&self) -> bool {
            // A maximal I4 pair is 17 header + 2*21 row bytes. Leave a
            // second record for the longest ACK1 control and both delimiters;
            // pause must still be able to seal its final row and acknowledge.
            self.records.len() + 2 <= OUTPUT_RECORD_LIMIT
                && self.bytes + 59 + 32 + 2 <= OUTPUT_BYTE_LIMIT
        }

        fn push(&mut self, wire: String) -> io::Result<()> {
            if self.records.len() >= OUTPUT_RECORD_LIMIT
                || self.bytes + wire.len() + 1 > OUTPUT_BYTE_LIMIT
            {
                return Err(io::Error::other(
                    "journal output queue capacity exceeded (120 records / 2048 bytes); no queued record overwritten; helper stopped",
                ));
            }
            self.bytes += wire.len() + 1;
            self.records.push_back(wire);
            Ok(())
        }

        fn pop(&mut self) -> Option<String> {
            let wire = self.records.pop_front()?;
            self.bytes -= wire.len() + 1;
            Some(wire)
        }

        fn is_empty(&self) -> bool {
            self.records.is_empty()
        }
    }

    const TEXT_WINDOW: usize = 16;
    const TEXT_RETRY: Duration = Duration::from_millis(250);

    #[derive(Default)]
    struct TextWindow {
        received: u32,
        consumed: u32,
        highest_sent: u32,
        next_sequence: u32,
        retry_at: Option<Instant>,
        retry_permit: bool,
        recovery_through: u32,
        receipt_revision: u32,
        retried_revision: u32,
    }

    impl TextWindow {
        fn receipt(&mut self, queue: &mut PendingOutput, received: u32, consumed: u32, revision: u32) -> io::Result<()> {
            if received > self.highest_sent || consumed > received {
                return Err(io::Error::other("native text receipt acknowledges an unsent or unreceived record"));
            }
            if received > self.received {
                self.received = received;
                self.retry_at = None;
                self.retry_permit = false;
                if received < self.recovery_through {
                    self.next_sequence = received + 1;
                    self.retry_permit = true;
                } else {
                    self.recovery_through = 0;
                }
            }
            self.next_sequence = self.next_sequence.max(self.received + 1);
            if consumed > self.consumed {
                let count = (consumed - self.consumed) as usize;
                if count > queue.records.len() {
                    return Err(io::Error::other("native consumed receipt exceeds retained records"));
                }
                for _ in 0..count {
                    queue.pop();
                }
                self.consumed = consumed;
            }
            if self.next_sequence <= self.consumed {
                self.next_sequence = self.consumed + 1;
            }
            if queue.is_empty() {
                self.retry_at = None;
            }
            self.receipt_revision = self.receipt_revision.max(revision);
            Ok(())
        }

        fn acknowledge(&mut self, queue: &mut PendingOutput, sequence: u32) -> io::Result<()> {
            self.receipt(queue, sequence, sequence, self.receipt_revision)
        }

        fn next(
            &mut self,
            queue: &PendingOutput,
            epoch: u32,
            now: Instant,
        ) -> io::Result<Option<(u32, String)>> {
            if self.next_sequence == 0 {
                self.next_sequence = 1;
            }
            if self.highest_sent > self.received {
                self.retry_at.get_or_insert(now + TEXT_RETRY);
            } else {
                self.retry_at = None;
            }
            // Time alone cannot distinguish lost text from a stopped receiver.
            // Spend each fresh native receipt at most once on retransmission.
            if self.retry_at.is_some_and(|deadline| now >= deadline)
                && self.receipt_revision > self.retried_revision
            {
                self.next_sequence = self.received + 1;
                self.retry_at = None;
                self.retried_revision = self.receipt_revision;
                self.retry_permit = true;
                self.recovery_through = self.highest_sent;
            }
            if self.next_sequence <= self.highest_sent && !self.retry_permit {
                return Ok(None);
            }
            if self.next_sequence > self.consumed + TEXT_WINDOW as u32
                || self.next_sequence <= self.consumed
                || (self.next_sequence - self.consumed - 1) as usize >= queue.records.len()
            {
                return Ok(None);
            }
            let sequence = self.next_sequence;
            if sequence >= i32::MAX as u32 {
                return Err(io::Error::other("text record sequence exhausted"));
            }
            let index = (sequence - self.consumed - 1) as usize;
            Ok(Some((
                sequence,
                text_envelope(epoch, sequence, &queue.records[index])?,
            )))
        }

        fn sent(&mut self, sequence: u32, now: Instant) {
            if sequence <= self.highest_sent {
                self.retry_permit = false;
            }
            self.highest_sent = self.highest_sent.max(sequence);
            self.next_sequence = sequence + 1;
            self.retry_at.get_or_insert(now + TEXT_RETRY);
        }

        fn suspend(&mut self) {
            self.next_sequence = self.received + 1;
            self.retry_at = None;
            self.recovery_through = self.highest_sent;
            self.retry_permit = self.received < self.highest_sent;
        }
    }

    fn text_envelope(epoch: u32, sequence: u32, wire: &str) -> io::Result<String> {
        let identity = format!("{epoch:010}{sequence:010}");
        let mut checksum = 0u32;
        for byte in identity
            .bytes()
            .chain(std::iter::once(b'|'))
            .chain(wire.bytes())
        {
            let symbol = if byte == b'|' {
                Some(64)
            } else {
                ALPHABET.iter().position(|value| *value == byte)
            };
            let symbol =
                symbol.ok_or_else(|| io::Error::other("unsupported text record character"))?;
            checksum = (checksum * 251 + symbol as u32 + 1) % 65_521;
        }
        Ok(format!("@J1{identity}{checksum:05}|{wire};"))
    }

    #[derive(Debug, PartialEq, Eq)]
    struct TextReceipt {
        received: u32,
        consumed: u32,
        revision: u32,
        chat: u32,
        chat_state: u32,
    }

    fn text_receipt(contents: &str, build: &str, epoch: u32, slot: u32) -> io::Result<Option<TextReceipt>> {
        if contents
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .map(str::trim)
            != Some("endfunction")
        {
            return Ok(None);
        }
        let prefix =
            format!("SMASHCRAFT TEXT ACK v=1 build={build} epoch={epoch} slot={slot} received=");
        let Some(start) = contents.find(&prefix) else {
            return Err(io::Error::other("native text receipt identity mismatch"));
        };
        let tail = &contents[start + prefix.len()..];
        let number: String = tail.chars().take_while(char::is_ascii_digit).collect();
        let Some(consumed_text) = tail[number.len()..].strip_prefix(" consumed=") else {
            return Err(io::Error::other("malformed native text receipt"));
        };
        let consumed: String = consumed_text.chars().take_while(char::is_ascii_digit).collect();
        let Some(revision_text) = consumed_text[consumed.len()..].strip_prefix(" revision=") else {
            return Err(io::Error::other("malformed native text receipt"));
        };
        let revision: String = revision_text.chars().take_while(char::is_ascii_digit).collect();
        let fields = revision_text[revision.len()..].split('"').next().unwrap_or("");
        let chat_fields: Vec<_> = fields.split_whitespace().collect();
        if chat_fields.len() != 3 || number.is_empty() || consumed.is_empty() || revision.is_empty() {
            return Err(io::Error::other("malformed native text receipt"));
        }
        let counter = |text: &str| text.parse::<u32>()
            .map_err(|_| io::Error::other("native text receipt counter overflow"));
        let chat = counter(chat_fields[0].strip_prefix("chat=").ok_or_else(|| io::Error::other("missing chat sequence"))?)?;
        let chat_state = counter(chat_fields[1].strip_prefix("chatState=").ok_or_else(|| io::Error::other("missing chat state"))?)?;
        if chat_state > 3 || (chat == 0 && chat_state != 0)
            || !matches!(chat_fields[2], "chatFrame=0" | "chatFrame=1") {
            return Err(io::Error::other("invalid native chat receipt"));
        }
        Ok(Some(TextReceipt { received: counter(&number)?, consumed: counter(&consumed)?, revision: counter(&revision)?, chat, chat_state }))
    }

    /// Where the helper types: the game's window, after its focus checks, or
    /// a file a headless client reads, one line per typing.
    enum Typist {
        Window {
            output: Enigo,
            gate: crate::focus::Gate,
            window: u32,
        },
        File(fs::File),
    }

    impl Typist {
        fn window(display: &str, target: crate::focus::Target) -> Result<Self, String> {
            let window = target.window;
            let gate = crate::focus::Gate::new(target)?;
            let settings = Settings {
                x11_display: Some(display.to_owned()),
                linux_delay: 0,
                release_keys_when_dropped: false,
                ..Settings::default()
            };
            let output = Enigo::new(&settings).map_err(|error| error.to_string())?;
            Ok(Self::Window { output, gate, window })
        }

        fn file(path: &Path) -> Result<Self, String> {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map(Self::File)
                .map_err(|error| format!("open text output {}: {error}", path.display()))
        }

        /// A headless client has no window to lose focus.
        fn eligible(&mut self) -> Result<bool, String> {
            match self {
                Self::Window { gate, .. } => gate.eligible(),
                Self::File(_) => Ok(true),
            }
        }

        fn away(&self) -> Option<&str> {
            match self {
                Self::Window { gate, .. } => gate.away(),
                Self::File(_) => None,
            }
        }

        fn text(&mut self, text: &str) -> io::Result<()> {
            match self {
                Self::Window { output, window, .. } => output
                    .text_to_window(text, *window)
                    .map_err(|error| io::Error::other(error.to_string())),
                // One write per line, so a reader never sees half of one.
                Self::File(file) => file.write_all(format!("{text}\n").as_bytes()),
            }
        }

        fn key(&mut self, key: OutputKey, direction: Direction) -> io::Result<()> {
            match self {
                Self::Window { output, .. } => output
                    .key(key, direction)
                    .map_err(|error| io::Error::other(error.to_string())),
                Self::File(_) => Err(io::Error::other(
                    "text output types text only; chat and the keyboard mailbox need a game window",
                )),
            }
        }
    }

    #[test]
    fn text_output_appends_one_line_per_typing_and_types_no_keys() {
        let path = env::temp_dir().join(format!("journal-text-out-{}", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut typist = Typist::file(&path).unwrap();
        assert!(typist.eligible().unwrap());
        typist.text("@J1a;").unwrap();
        typist.text("n").unwrap();
        assert!(typist.key(OutputKey::Return, Direction::Press).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "@J1a;\nn\n");
        fs::remove_file(path).unwrap();
    }

    const MAILBOX_CHUNK_BYTES: usize = 7;
    const MAILBOX_SIGNAL_COUNT: usize = 54;

    struct MailboxSender {
        dir: PathBuf,
        build: String,
        epoch: u32,
        slot: u32,
        typist: Typist,
        owned: BTreeSet<usize>,
        queued: PendingOutput,
        text_window: TextWindow,
        text_receipt_at: Option<Instant>,
        focus: FocusGrace,
        current: Option<String>,
        offset: usize,
        next_chunk: u32,
        toggle: bool,
        awaiting_ack: bool,
        trace: bool,
        emitted_at: Option<Instant>,
        editbox: bool,
        chat: u32,
        chat_state: u32,
        chat_quiet: u32,
        chat_opened: u32,
    }

    impl MailboxSender {
        fn new(
            dir: &Path,
            build: &str,
            epoch: u32,
            slot: u32,
            trace: bool,
            editbox: bool,
            typist: Typist,
        ) -> Result<Self, String> {
            Ok(Self {
                dir: dir.to_owned(),
                build: build.to_owned(),
                epoch,
                slot,
                typist,
                owned: BTreeSet::new(),
                queued: PendingOutput::default(),
                text_window: TextWindow::default(),
                text_receipt_at: None,
                focus: FocusGrace::default(),
                current: None,
                offset: 0,
                next_chunk: 1,
                toggle: false,
                awaiting_ack: false,
                trace,
                emitted_at: None,
                editbox,
                chat: 0,
                chat_state: 0,
                chat_quiet: 0,
                chat_opened: 0,
            })
        }

        fn enqueue(&mut self, wire: String) -> io::Result<()> {
            if wire.is_empty() || !wire.bytes().all(|byte| (32..=126).contains(&byte)) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "mailbox wire must be printable ASCII",
                ));
            }
            if self.editbox
                && !wire
                    .bytes()
                    .all(|byte| ALPHABET.contains(&byte) || byte == b'|')
            {
                return Err(io::Error::other(
                    "controller record contains an unsupported character",
                ));
            }
            self.queued.push(wire)
        }

        /// Whether the game has focus now. Typing always requires it.
        fn eligible(&mut self) -> io::Result<bool> {
            let eligible = self.typist.eligible().map_err(io::Error::other)?;
            let now = monotonic_ns()?;
            match self.focus.observe(eligible, now) {
                Some(FocusChange::Away) => eprintln!(
                    "focus_away mono_ns={now} reason={}",
                    self.typist.away().unwrap_or("unknown")
                ),
                Some(FocusChange::Back { away_ns, lost }) => eprintln!(
                    "focus_back mono_ns={now} away_us={} focus_loss={}",
                    away_ns / 1_000,
                    if lost { "released" } else { "blip-input-kept" }
                ),
                None => {}
            }
            Ok(eligible)
        }

        /// Whether captured input stays armed: focus is here or left less than
        /// the grace period ago.
        fn focused(&mut self) -> io::Result<bool> {
            self.eligible()?;
            Ok(self.focus.held(monotonic_ns()?))
        }

        fn ack_path(&self) -> PathBuf {
            mailbox_ack_path(
                &self.dir,
                &self.build,
                self.epoch,
                self.slot,
                self.next_chunk,
            )
        }

        fn is_idle(&self) -> bool {
            self.queued.is_empty() && self.current.is_none() && !self.awaiting_ack
        }

        fn step(&mut self, paused: bool) -> io::Result<()> {
            if self.editbox {
                let now = Instant::now();
                if self.text_receipt_at.is_none_or(|previous| {
                    now.duration_since(previous) >= Duration::from_millis(10)
                }) {
                    let path = self.dir.join(format!(
                        "smashcraft-journal-text-ack-{}-e{}-p{}.txt",
                        self.build, self.epoch, self.slot
                    ));
                    match fs::read_to_string(path) {
                        Ok(contents) => {
                            if let Some(receipt) =
                                text_receipt(&contents, &self.build, self.epoch, self.slot)?
                            {
                                let TextReceipt { received, consumed, revision, chat, chat_state } = receipt;
                                let previous_revision = self.text_window.receipt_revision;
                                self.text_window.receipt(&mut self.queued, received, consumed, revision)?;
                                if revision > previous_revision {
                                    if (chat, chat_state) != (self.chat, self.chat_state) {
                                        eprintln!("chat_state epoch={} chat={chat} state={chat_state} mono_ns={}", self.epoch, monotonic_ns()?);
                                    }
                                    self.chat = chat;
                                    self.chat_state = chat_state;
                                }
                                if self.trace && revision > previous_revision {
                                    eprintln!(
                                        "editbox_receipt monotonic_ns={} received={received} consumed={consumed} revision={revision}",
                                        monotonic_ns()?
                                    );
                                }
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error),
                    }
                    self.text_receipt_at = Some(now);
                }
                if !self.eligible()? {
                    self.text_window.suspend();
                    return Ok(());
                }
                if self.chat_state == 1 && paused && self.is_idle() && self.chat_quiet != self.chat {
                    self.chat_quiet = self.chat;
                    let path = self.dir.join(format!("smashcraft-journal-chat-{}-e{}-s{}-n{}.pld", self.build, self.epoch, self.slot, self.chat));
                    publish_symbol(&path, b'Q')?;
                    eprintln!("chat_quiescent epoch={} chat={} mono_ns={}", self.epoch, self.chat, monotonic_ns()?);
                }
                if self.chat_state == 2 && self.chat_opened != self.chat {
                    if self.chat_quiet != self.chat || !self.is_idle() {
                        return Err(io::Error::other("native chat opened before sender quiescence"));
                    }
                    self.typist.key(OutputKey::Return, Direction::Press)?;
                    thread::sleep(Duration::from_millis(30));
                    self.typist.key(OutputKey::Return, Direction::Release)?;
                    self.chat_opened = self.chat;
                    eprintln!("chat_return epoch={} chat={} mono_ns={}", self.epoch, self.chat, monotonic_ns()?);
                }
                if self.chat_state >= 2 || (self.chat_state == 1 && self.chat_quiet == self.chat) {
                    return Ok(());
                }
                if let Some((sequence, envelope)) =
                    self.text_window.next(&self.queued, self.epoch, now)?
                {
                    let started = Instant::now();
                    self.typist.text(&envelope)?;
                    self.text_window.sent(sequence, now);
                    if self.trace {
                        eprintln!(
                            "editbox_emit sequence={sequence} bytes={} elapsed_us={} envelope={envelope}",
                            envelope.len(),
                            started.elapsed().as_micros()
                        );
                    }
                }
                return Ok(());
            }
            if self.awaiting_ack {
                if !mailbox_ack_matches(
                    &self.ack_path(),
                    &self.build,
                    self.epoch,
                    self.slot,
                    self.next_chunk,
                )? {
                    return Ok(());
                }
                if self.trace {
                    if let Some(emitted_at) = self.emitted_at.take() {
                        eprintln!(
                            "mailbox_ack chunk={} wait_us={}",
                            self.next_chunk,
                            emitted_at.elapsed().as_micros()
                        );
                    }
                }
                fs::remove_file(self.ack_path())?;
                self.awaiting_ack = false;
                self.next_chunk += 1;
                if self
                    .current
                    .as_ref()
                    .is_some_and(|wire| self.offset >= wire.len())
                {
                    self.current = None;
                    self.offset = 0;
                }
            }
            if self.current.is_none() {
                self.current = self.queued.pop();
                if self.current.is_none() {
                    return Ok(());
                }
            }
            let wire = self.current.as_ref().expect("current mailbox message");
            let bytes = wire.as_bytes();
            let end = (self.offset + MAILBOX_CHUNK_BYTES).min(bytes.len());
            let chunk = bytes[self.offset..end].to_vec();
            let final_chunk = end == bytes.len();
            let signals = mailbox_signal_values(&chunk, final_chunk, !self.toggle)?;
            let started = self.trace.then(Instant::now);
            let transitions = if self.trace {
                (0..MAILBOX_SIGNAL_COUNT)
                    .filter(|signal| self.owned.contains(signal) != signals[*signal])
                    .count()
            } else {
                0
            };

            // Change payload and framing while the old commit toggle remains
            // stable. Flip that key last so the game never samples a partial
            // chunk as committed.
            for signal in 0..MAILBOX_SIGNAL_COUNT - 1 {
                if !self.set_signal(signal, signals[signal])? {
                    return Ok(());
                }
            }
            if !self.set_signal(53, signals[53])? {
                return Ok(());
            }
            self.toggle = !self.toggle;
            self.offset = end;
            self.awaiting_ack = true;
            if let Some(started) = started {
                self.emitted_at = Some(Instant::now());
                eprintln!(
                    "mailbox_emit chunk={} bytes={} transitions={} elapsed_us={}",
                    self.next_chunk,
                    chunk.len(),
                    transitions,
                    started.elapsed().as_micros(),
                );
            }
            Ok(())
        }

        fn set_signal(&mut self, signal: usize, down: bool) -> io::Result<bool> {
            let was_down = self.owned.contains(&signal);
            if was_down == down {
                return Ok(true);
            }
            if !self.eligible()? {
                return Ok(false);
            }
            self.typist.key(
                mailbox_output_key(signal),
                if down {
                    Direction::Press
                } else {
                    Direction::Release
                },
            )?;
            if down {
                self.owned.insert(signal);
            } else {
                self.owned.remove(&signal);
            }
            Ok(true)
        }

        fn release_all(&mut self) {
            for signal in self.owned.clone() {
                let _ = self.set_signal(signal, false);
            }
        }
    }

    impl Drop for MailboxSender {
        fn drop(&mut self) {
            self.release_all();
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct FrameSegment {
        epoch_ns: u128,
        first_frame: u32,
    }

    fn frame_through(segment: FrameSegment, at_ns: u128) -> u32 {
        if at_ns < segment.epoch_ns {
            return segment.first_frame.saturating_sub(1);
        }
        ((((at_ns - segment.epoch_ns) * HZ / 1_000_000_000)
            + u128::from(segment.first_frame)
            - 1)
            .min(LAST_FRAME as u128)) as u32
    }

    fn pause_seal_frontier(
        segment: FrameSegment,
        published_ns: u128,
        requested_frame: u32,
        assigned_through: u32,
    ) -> u32 {
        frame_through(segment, published_ns)
            .max(requested_frame.saturating_sub(1))
            .max(assigned_through)
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum ControlState {
        Started,
        Ended,
        PausePrepare,
        PauseCommit,
        Paused,
        Resumed,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct ControlCommand {
        build: String,
        epoch: u32,
        slot: u32,
        sequence: u32,
        state: ControlState,
        requested_frame: u32,
    }

    fn usage() -> &'static str {
        "wc3-journal --follow-matches --build BUILD --slot N --device /dev/input/eventN --out DIR --editbox-display :N [--trace]\n\
         Start in character selection; stick left/right chooses, A selects, X backs, Start confirms. Follows matches and rematches.\n\
         Diagnostic only: wc3-journal --device /dev/input/eventN --out DIR --ready-file PATH --epoch-monotonic-ns NS [--mailbox-display :N | --editbox-display :N] [--first-frame N] [--stop-frame N] [--trace]\n\
         Keyboard output also requires --x11-window DECIMAL_ID --pid PID and exactly one of --niri-window ID / --private-wlr-app-id ID.\n\
         Headless clients: --text-out FILE instead of --editbox-display appends each typing to FILE as one line, with no window or focus checks.\n\
         Assigns Linux kernel CLOCK_MONOTONIC input_event times to half-open 60 Hz frames. The capture segment starts at the explicit host monotonic epoch; first-frame defaults to 1."
    }

    fn mailbox_ack_path(dir: &Path, build: &str, epoch: u32, slot: u32, chunk: u32) -> PathBuf {
        dir.join(format!(
            "smashcraft-journal-mailbox-ack-{build}-e{epoch}-s{slot}-c{chunk}.txt"
        ))
    }

    fn mailbox_ack_matches(
        path: &Path,
        build: &str,
        epoch: u32,
        slot: u32,
        chunk: u32,
    ) -> io::Result<bool> {
        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        if contents
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .map(str::trim)
            != Some("endfunction")
        {
            return Ok(false);
        }
        let fields = contents
            .split_whitespace()
            .filter_map(|token| token.trim_matches('"').split_once('='));
        let fields = fields.collect::<BTreeMap<_, _>>();
        Ok(fields.get("v") == Some(&"1")
            && fields.get("build") == Some(&build)
            && fields
                .get("epoch")
                .and_then(|value| value.parse::<u32>().ok())
                == Some(epoch)
            && fields
                .get("slot")
                .and_then(|value| value.parse::<u32>().ok())
                == Some(slot)
            && fields
                .get("chunk")
                .and_then(|value| value.parse::<u32>().ok())
                == Some(chunk))
    }

    fn mailbox_virtual_key(signal: usize) -> u16 {
        match signal {
            0..=9 => 0x30 + signal as u16,
            10..=33 => 0x41 + (signal - 10) as u16,
            34 => 0x5A,
            35..=45 => [
                0xBD, 0xBB, 0xDB, 0xDD, 0xDC, 0xBA, 0xDE, 0xBC, 0xBE, 0xBF, 0xC0,
            ][signal - 35],
            46..=53 => 0x7C + (signal - 46) as u16,
            _ => unreachable!("mailbox signal is in 0..54"),
        }
    }

    fn mailbox_signal_values(
        chunk: &[u8],
        final_chunk: bool,
        toggle: bool,
    ) -> io::Result<[bool; MAILBOX_SIGNAL_COUNT]> {
        if chunk.is_empty() || chunk.len() > MAILBOX_CHUNK_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid keyboard mailbox chunk",
            ));
        }
        let mut signals = [false; MAILBOX_SIGNAL_COUNT];
        for (offset, byte) in chunk.iter().enumerate() {
            if !(32..=126).contains(byte) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "keyboard mailbox requires printable ASCII",
                ));
            }
            for bit in 0..7 {
                signals[offset * 7 + bit] = byte & (1 << bit) != 0;
            }
        }
        for bit in 0..3 {
            signals[49 + bit] = chunk.len() & (1 << bit) != 0;
        }
        signals[52] = final_chunk;
        signals[53] = toggle;
        Ok(signals)
    }

    fn mailbox_output_key(signal: usize) -> OutputKey {
        match mailbox_virtual_key(signal) {
            vk @ 0x30..=0x39 => OutputKey::Unicode(char::from(vk as u8)),
            vk @ 0x41..=0x5A => OutputKey::Unicode(char::from(vk as u8 + 32)),
            0xBD => OutputKey::Unicode('-'),
            0xBB => OutputKey::Unicode('='),
            0xDB => OutputKey::Unicode('['),
            0xDD => OutputKey::Unicode(']'),
            0xDC => OutputKey::Unicode('\\'),
            0xBA => OutputKey::Unicode(';'),
            0xDE => OutputKey::Unicode('\''),
            0xBC => OutputKey::Unicode(','),
            0xBE => OutputKey::Unicode('.'),
            0xBF => OutputKey::Unicode('/'),
            0xC0 => OutputKey::Unicode('`'),
            0x7C => OutputKey::F13,
            0x7D => OutputKey::F14,
            0x7E => OutputKey::F15,
            0x7F => OutputKey::F16,
            0x80 => OutputKey::F17,
            0x81 => OutputKey::F18,
            0x82 => OutputKey::F19,
            0x83 => OutputKey::F20,
            _ => unreachable!("mailbox key mapping is complete"),
        }
    }

    fn options() -> Result<Options, String> {
        let mut values = BTreeMap::<String, String>::new();
        let mut trace = false;
        let mut follow_matches = false;
        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            if arg == "--help" || arg == "-h" {
                println!("{}", usage());
                std::process::exit(0);
            }
            if arg == "--follow-matches" {
                follow_matches = true;
                continue;
            }
            if arg == "--trace" {
                trace = true;
                continue;
            }
            if !arg.starts_with("--") {
                return Err(format!("unexpected argument {arg:?}\n{}", usage()));
            }
            let value = args
                .next()
                .ok_or_else(|| format!("missing value for {arg}"))?;
            values.insert(arg, value);
        }
        let take = |key: &str| {
            values
                .get(key)
                .ok_or_else(|| format!("missing {key}\n{}", usage()))
        };
        let parse = |key: &str| -> Result<u32, String> {
            take(key)?.parse().map_err(|_| format!("invalid {key}"))
        };
        let epoch_ns = if follow_matches { 0 } else {
            take("--epoch-monotonic-ns")?.parse::<u128>()
                .map_err(|_| "invalid --epoch-monotonic-ns")?
        };
        if follow_matches && (values.contains_key("--ready-file") || values.contains_key("--epoch-monotonic-ns") || values.contains_key("--stop-frame") || values.contains_key("--first-frame")) {
            return Err("--follow-matches cannot use diagnostic ready/epoch/frame arguments".into());
        }
        if follow_matches && !values.contains_key("--editbox-display") && !values.contains_key("--text-out") {
            return Err("--follow-matches requires --editbox-display or --text-out".into());
        }
        let ready_file = values.get("--ready-file").map(PathBuf::from);
        let (build, epoch, slot, delay) = if follow_matches {
            (take("--build")?.clone(), 0, parse("--slot")?, 0)
        } else if let Some(path) = ready_file.as_ref() {
            read_ready(path)?
        } else {
            (
                take("--build")?.clone(),
                parse("--epoch")?,
                parse("--slot")?,
                parse("--delay")?,
            )
        };
        if build.is_empty()
            || build.len() > 80
            || !build
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err("--build must be 1..80 ASCII letters, digits, '-' or '_'".into());
        }
        Ok(Options {
            device: take("--device")?.into(),
            out: take("--out")?.into(),
            build,
            epoch,
            slot,
            delay,
            epoch_ns,
            first_frame: values
                .get("--first-frame")
                .map(|s| s.parse().map_err(|_| "invalid --first-frame"))
                .transpose()?
                .unwrap_or(1),
            stop_frame: values
                .get("--stop-frame")
                .map(|s| s.parse().map_err(|_| "invalid --stop-frame"))
                .transpose()?,
            ready_file,
            mailbox_display: values.get("--mailbox-display").cloned(),
            editbox_display: values.get("--editbox-display").cloned(),
            text_out: values.get("--text-out").map(PathBuf::from),
            trace,
            follow_matches,
            window: values
                .get("--x11-window")
                .map(|s| s.parse().map_err(|_| "invalid --x11-window"))
                .transpose()?,
            pid: values
                .get("--pid")
                .map(|s| s.parse().map_err(|_| "invalid --pid"))
                .transpose()?,
            niri_window: values
                .get("--niri-window")
                .map(|s| s.parse().map_err(|_| "invalid --niri-window"))
                .transpose()?,
            wlr_app_id: values.get("--private-wlr-app-id").cloned(),
        })
    }

    fn lifecycle_path(dir: &Path, build: &str, epoch: u32, slot: u32, kind: &str) -> PathBuf {
        dir.join(format!("smashcraft-journal-{kind}-{build}-e{epoch}-s{slot}.txt"))
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum MenuPhase { Character, Stage, Result }

    fn menu_phase(contents: &str, build: &str, epoch: u32, slot: u32) -> Option<MenuPhase> {
        if contents.lines().rev().find(|line| !line.trim().is_empty()).map(str::trim) != Some("endfunction") {
            return None;
        }
        let prefix = format!("SMASHCRAFT JOURNAL MENU v=1 build={build} epoch={epoch} slot={slot} phase=");
        let value = contents.split_once(&prefix)?.1.split_once('"')?.0;
        match value {
            "CHARACTER" => Some(MenuPhase::Character),
            "STAGE" => Some(MenuPhase::Stage),
            "RESULT" => Some(MenuPhase::Result),
            _ => None,
        }
    }

    fn fresh_menu(dir: &Path, build: &str, epoch: u32, slot: u32, after_ns: u128, now_ns: u128) -> Result<Option<MenuPhase>, String> {
        let path = dir.join(format!("smashcraft-journal-menu-{build}-s{slot}.txt"));
        let mut file = match fs::File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("open menu publication: {error}")),
        };
        let before = file.metadata().map_err(|e| e.to_string())?;
        let modified = before.modified().map_err(|e| e.to_string())?.duration_since(UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos();
        // Map refreshes eligible menus every 250 ms; an exited or frozen map
        // cannot leave an indefinitely valid permission to send keys.
        if modified <= after_ns || modified > now_ns || now_ns - modified > 1_000_000_000 {
            return Ok(None);
        }
        let mut contents = String::new();
        file.read_to_string(&mut contents).map_err(|e| e.to_string())?;
        let after = file.metadata().map_err(|e| e.to_string())?;
        let named = fs::metadata(path).map_err(|e| e.to_string())?;
        if !same_control_file(&before, &after) || !same_control_file(&after, &named) || after.len() != contents.len() as u64 {
            return Ok(None);
        }
        Ok(menu_phase(&contents, build, epoch, slot))
    }

    fn menu_buttons(physical: State, start: bool) -> u32 {
        (action_state(physical) & (MOVE_LEFT | MOVE_RIGHT | ATTACK | SPECIAL)) | (u32::from(start) << 31)
    }

    #[derive(Default)]
    struct MenuInput {
        phase: Option<MenuPhase>,
        armed: bool,
        accept_since_ns: u128,
    }

    impl MenuInput {
        fn observe(&mut self, phase: Option<MenuPhase>, physical: State, start: bool, now: u128) {
            if self.phase != phase {
                self.phase = phase;
                self.armed = false;
                self.accept_since_ns = now;
            }
            if phase.is_none() {
                self.armed = false;
            } else if action_state(physical) == 0 && !start {
                self.armed = true;
            }
        }

        fn press(&mut self, before: State, after: State, start_before: bool, start_after: bool, event_time: u128) -> Option<&'static str> {
            if self.phase.is_none() || event_time < self.accept_since_ns {
                return None;
            }
            if !self.armed {
                self.armed = action_state(after) == 0 && !start_after;
                return None;
            }
            let pressed = menu_buttons(after, start_after) & !menu_buttons(before, start_before);
            // A physical event changes only one mapped menu control.
            match pressed {
                MOVE_LEFT => Some("w"),
                MOVE_RIGHT => Some("r"),
                ATTACK => Some("n"),
                SPECIAL => Some("u"),
                0x8000_0000 => Some("y"),
                _ => None,
            }
        }
    }

    #[test]
    fn menu_requires_fresh_complete_matching_map_permission() {
        let dir = env::temp_dir().join(format!("journal-menu-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("smashcraft-journal-menu-menu-test-s1.txt");
        let body = "call Preload( \"SMASHCRAFT JOURNAL MENU v=1 build=menu-test epoch=2 slot=1 phase=RESULT\" )\n";
        fs::write(&path, body).unwrap();
        let now = clock_ns(libc::CLOCK_REALTIME).unwrap();
        assert_eq!(fresh_menu(&dir, "menu-test", 2, 1, 0, now).unwrap(), None);
        fs::write(&path, format!("{body}endfunction\n")).unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        assert_eq!(fresh_menu(&dir, "menu-test", 2, 1, 0, modified).unwrap(), Some(MenuPhase::Result));
        assert_eq!(fresh_menu(&dir, "menu-test", 1, 1, 0, modified).unwrap(), None);
        assert_eq!(fresh_menu(&dir, "menu-test", 2, 1, modified, modified).unwrap(), None);
        assert_eq!(fresh_menu(&dir, "menu-test", 2, 1, 0, modified + 1_000_000_001).unwrap(), None);
        assert_eq!(menu_phase(&format!("{body}endfunction\n"), "other", 2, 1), None);
        assert_eq!(menu_phase(&format!("{body}endfunction\n"), "menu-test", 2, 0), None);
        fs::write(&path, format!("{}endfunction\n", body.replace("RESULT", "BLOCKED"))).unwrap();
        assert_eq!(fresh_menu(&dir, "menu-test", 2, 1, 0, clock_ns(libc::CLOCK_REALTIME).unwrap()).unwrap(), None);
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn menu_taps_require_edges_and_neutral_across_phase_and_focus_changes() {
        let neutral = State::default();
        let attack = State { sources: 1, ..neutral };
        let left = State { x: -10_000, ..neutral };
        let right = State { x: 10_000, ..neutral };
        let mut input = MenuInput::default();
        assert_eq!(input.press(neutral, attack, false, false, 1), None);
        input.observe(Some(MenuPhase::Character), attack, false, 10);
        assert_eq!(input.press(attack, attack, false, false, 11), None);
        assert_eq!(input.press(attack, neutral, false, false, 12), None);
        assert_eq!(input.press(neutral, attack, false, false, 13), Some("n"));
        assert_eq!(input.press(attack, attack, false, false, 14), None);
        assert_eq!(input.press(neutral, left, false, false, 15), Some("w"));
        assert_eq!(input.press(left, left, false, false, 16), None);
        assert_eq!(input.press(left, right, false, false, 17), Some("r"));
        assert_eq!(input.press(neutral, State { sources: 4, ..neutral }, false, false, 18), Some("u"));
        assert_eq!(input.press(neutral, neutral, false, true, 19), Some("y"));
        input.observe(Some(MenuPhase::Stage), neutral, true, 20);
        assert_eq!(input.press(neutral, attack, true, true, 21), None);
        assert_eq!(input.press(attack, neutral, true, false, 22), None);
        assert_eq!(input.press(neutral, neutral, false, true, 23), Some("y"));
        // Closed menus and focus loss use the same revocation. A held-through
        // return button cannot confirm, nor can an old queued press.
        input.observe(None, neutral, false, 30);
        assert_eq!(input.press(neutral, attack, false, false, 31), None);
        input.observe(Some(MenuPhase::Result), attack, false, 40);
        assert_eq!(input.press(attack, attack, false, false, 41), None);
        assert_eq!(input.press(attack, neutral, false, false, 42), None);
        assert_eq!(input.press(neutral, attack, false, false, 39), None);
        assert_eq!(input.press(neutral, attack, false, false, 43), Some("n"));
    }

    fn quiescent_path(dir: &Path, build: &str, epoch: u32, slot: u32) -> PathBuf {
        dir.join(format!("smashcraft-journal-quiescent-{build}-e{epoch}-s{slot}.pld"))
    }

    fn clear_quiescent(dir: &Path, build: &str, epoch: u32, slot: u32) -> Result<(), String> {
        match fs::remove_file(quiescent_path(dir, build, epoch, slot)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("clear prior helper quiescence: {error}")),
        }
    }

    fn validate_lifecycle(command: &ControlCommand, o: &Options, state: ControlState) -> Result<(), String> {
        if command.build != o.build || command.epoch != o.epoch || command.slot != o.slot
            || command.sequence != 0 || command.state != state
            || (state == ControlState::Started && command.requested_frame != 1 + o.delay) {
            return Err("match lifecycle publication does not match active session".into());
        }
        Ok(())
    }

    fn fresh_ready(dir: &Path, build: &str, slot: u32, last_epoch: u32, after_ns: u128) -> Result<Option<(u32, u32, u128)>, String> {
        let prefix = format!("smashcraft-journal-ready-{build}-e");
        let suffix = format!("-p{slot}.txt");
        let mut selected = None;
        for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(epoch) = name.strip_prefix(&prefix).and_then(|s| s.strip_suffix(&suffix)).and_then(|s| s.parse::<u32>().ok()) else { continue };
            if epoch <= last_epoch { continue; }
            let mut file = fs::File::open(entry.path()).map_err(|e| e.to_string())?;
            let before = file.metadata().map_err(|e| e.to_string())?;
            let modified = before.modified().map_err(|e| e.to_string())?.duration_since(UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos();
            if modified <= after_ns { continue; }
            let mut contents = String::new();
            file.read_to_string(&mut contents).map_err(|e| e.to_string())?;
            let after = file.metadata().map_err(|e| e.to_string())?;
            let named = fs::metadata(entry.path()).map_err(|e| e.to_string())?;
            if !same_control_file(&before, &after) || !same_control_file(&after, &named)
                || after.len() != contents.len() as u64
                || contents.lines().rev().find(|line| !line.trim().is_empty()).map(str::trim) != Some("endfunction") { continue; }
            let (receipt_build, receipt_epoch, receipt_slot, delay) = parse_ready(&contents)?;
            if receipt_build != build || receipt_epoch != epoch || receipt_slot != slot || delay > 64 {
                return Err("readiness filename and identity disagree".into());
            }
            if selected.is_none_or(|(previous, _, _)| epoch < previous) { selected = Some((epoch, delay, modified)); }
        }
        Ok(selected)
    }

    fn read_ready(path: &Path) -> Result<(String, u32, u32, u32), String> {
        let contents = fs::read_to_string(path).map_err(|error| {
            format!(
                "read native journal readiness receipt {}: {error}",
                path.display()
            )
        })?;
        parse_ready(&contents)
    }

    fn parse_ready(contents: &str) -> Result<(String, u32, u32, u32), String> {
        let mut fields = BTreeMap::<String, String>::new();
        for token in contents.split_whitespace() {
            let token = token.strip_suffix('"').unwrap_or(token);
            let Some((key, value)) = token.split_once('=') else {
                continue;
            };
            if let Some(previous) = fields.insert(key.into(), value.into()) {
                if previous != value {
                    return Err(format!("conflicting {key} values in readiness receipt"));
                }
            }
        }
        let value = |key: &str| {
            fields
                .get(key)
                .ok_or_else(|| format!("readiness receipt lacks {key}"))
        };
        let build = value("build")?.clone();
        let number = |key: &str| {
            value(key)?
                .parse::<u32>()
                .map_err(|_| format!("invalid {key} in readiness receipt"))
        };
        let epoch = number("epoch")?;
        let slot = number("slot")?;
        let delay = number("delay")?;
        if number("first_frame")? != 1 + delay {
            return Err("readiness receipt first_frame must equal 1 + delay".into());
        }
        Ok((build, epoch, slot, delay))
    }

    fn control_if_complete(contents: &str) -> Result<Option<ControlCommand>, String> {
        // PreloadGenEnd creates the file before finishing its contents. The
        // generated function's closing line is the publication boundary.
        if contents
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .map(str::trim)
            != Some("endfunction")
        {
            return Ok(None);
        }
        parse_control(contents).map(Some)
    }

    // Realtime and monotonic normally slew together. A changed relation,
    // including host suspend, invalidates this session's conversion.
    const CLOCK_RELATION_TOLERANCE_NS: u128 = 100_000;

    #[derive(Clone, Copy, Debug)]
    struct ControlClock {
        offset_ns: i128,
        uncertainty_ns: u128,
        timestamp_resolution_ns: u128,
    }

    impl ControlClock {
        fn sample(timestamp_resolution_ns: u128) -> io::Result<Self> {
            let before = monotonic_ns()?;
            let realtime = clock_ns(libc::CLOCK_REALTIME)?;
            let after = monotonic_ns()?;
            Ok(Self {
                offset_ns: realtime as i128 - ((before + after) / 2) as i128,
                uncertainty_ns: (after - before).div_ceil(2),
                timestamp_resolution_ns,
            })
        }

        fn new(directory: &Path) -> io::Result<Self> {
            let directory = fs::File::open(directory)?;
            let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
            // SAFETY: the fd is live and stat points to enough writable storage.
            if unsafe { libc::fstatfs(directory.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: successful fstatfs initialized stat.
            let kind = unsafe { stat.assume_init() }.f_type;
            // Local btrfs, XFS and tmpfs store subsecond timestamps.
            // Unknown/coarse/network filesystems do not share this contract.
            if !matches!(kind, 0x9123683e | 0x58465342 | 0x01021994) {
                return Err(io::Error::other(format!("unsupported control timestamp filesystem {kind:#x}")));
            }
            let mut resolution = libc::timespec { tv_sec: 0, tv_nsec: 0 };
            // SAFETY: resolution is a writable timespec.
            if unsafe { libc::clock_getres(libc::CLOCK_REALTIME_COARSE, &mut resolution) } != 0 {
                return Err(io::Error::last_os_error());
            }
            // File mtime may use the kernel coarse clock even when its stored
            // field has nanosecond precision.
            let resolution = resolution.tv_sec as u128 * 1_000_000_000 + resolution.tv_nsec as u128;
            Self::sample(resolution.max(1))
        }

        fn validate(self, current: Self) -> Result<(), String> {
            if self.offset_ns.abs_diff(current.offset_ns)
                > self.uncertainty_ns + current.uncertainty_ns + CLOCK_RELATION_TOLERANCE_NS
            {
                return Err("realtime/monotonic clock relation changed; resume timestamp cannot be reconstructed".into());
            }
            Ok(())
        }

        fn uncertainty(self) -> u128 {
            self.uncertainty_ns + self.timestamp_resolution_ns + CLOCK_RELATION_TOLERANCE_NS
        }

        fn publication(self, realtime_ns: u128, read_ns: u128) -> Result<u128, String> {
            let epoch = (realtime_ns as i128).checked_sub(self.offset_ns)
                .filter(|epoch| *epoch >= 0).ok_or("control publication predates monotonic clock")? as u128;
            if epoch > read_ns {
                return Err("control publication timestamp is in the future".into());
            }
            Ok(epoch)
        }
    }

    struct PublishedControl {
        command: ControlCommand,
        epoch_ns: u128,
        read_ns: u128,
        uncertainty_ns: u128,
    }

    struct ControlRead {
        publication: Option<PublishedControl>,
        // Absence proves that older paused events cannot belong to a future
        // publication. An incomplete file retains all pending input.
        discard_before_ns: u128,
    }

    fn same_control_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
        (a.dev(), a.ino(), a.len(), a.mtime(), a.mtime_nsec(), a.ctime(), a.ctime_nsec())
            == (b.dev(), b.ino(), b.len(), b.mtime(), b.mtime_nsec(), b.ctime(), b.ctime_nsec())
    }

    fn read_control(path: &Path, clock: ControlClock, published_after_ns: u128) -> Result<ControlRead, String> {
        let before_ns = monotonic_ns().map_err(|e| e.to_string())?;
        let current = ControlClock::sample(clock.timestamp_resolution_ns).map_err(|e| e.to_string())?;
        clock.validate(current)?;
        let mut file = match fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(ControlRead {
                    publication: None,
                    discard_before_ns: before_ns.saturating_sub(clock.uncertainty() + current.uncertainty_ns),
                });
            }
            Err(error) => return Err(format!("open journal control: {error}")),
        };
        let before = file.metadata().map_err(|e| e.to_string())?;
        let mut contents = String::new();
        file.read_to_string(&mut contents).map_err(|e| e.to_string())?;
        let after = file.metadata().map_err(|e| e.to_string())?;
        let named = fs::metadata(path).map_err(|e| e.to_string())?;
        let read_ns = monotonic_ns().map_err(|e| e.to_string())?;
        clock.validate(ControlClock::sample(clock.timestamp_resolution_ns).map_err(|e| e.to_string())?)?;
        let mut result = ControlRead { publication: None, discard_before_ns: 0 };
        if !same_control_file(&before, &after) || !same_control_file(&after, &named)
            || after.len() != contents.len() as u64
        {
            return Ok(result);
        }
        if let Some(command) = control_if_complete(&contents)? {
            let modified = after.modified().map_err(|e| e.to_string())?
                .duration_since(UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos();
            let epoch_ns = clock.publication(modified, read_ns)?;
            // A fresh game reuses build/epoch/sequence filenames. A complete
            // control from before this match's READY is absent for this match.
            if epoch_ns <= published_after_ns {
                result.discard_before_ns = before_ns.saturating_sub(clock.uncertainty() + current.uncertainty_ns);
                return Ok(result);
            }
            result.publication = Some(PublishedControl {
                command,
                epoch_ns,
                read_ns,
                uncertainty_ns: clock.uncertainty() + current.uncertainty_ns,
            });
        }
        Ok(result)
    }

    enum Capture {
        Event(evdev::InputEvent, bool, bool),
        Disconnected(u128),
        Reconnected { ns: u128, axes: [Option<evdev::AbsInfo>; 6], physical: State, start: bool },
    }

    impl Capture {
        fn timestamp(&self) -> Result<u128, String> {
            match self {
                Self::Event(event, ..) => event_ns(event),
                Self::Disconnected(ns) | Self::Reconnected { ns, .. } => Ok(*ns),
            }
        }
    }

    #[derive(Default)]
    struct PendingInput {
        events: VecDeque<Capture>,
    }

    impl PendingInput {
        fn push(&mut self, event: evdev::InputEvent, start_before: bool, start_after: bool) -> Result<(), String> {
            self.push_capture(Capture::Event(event, start_before, start_after))
        }

        fn push_capture(&mut self, capture: Capture) -> Result<(), String> {
            if self.events.len() >= 65_536 {
                return Err("unpublished control retained 65536 input events; helper stopped without discarding input".into());
            }
            self.events.push_back(capture);
            Ok(())
        }

        fn pop_for_output(&mut self, output_ready: bool, paused: bool, discard_before_ns: u128) -> Result<Option<Capture>, String> {
            if !output_ready {
                return Ok(None);
            }
            self.pop(paused, discard_before_ns)
        }

        fn pop(&mut self, paused: bool, discard_before_ns: u128) -> Result<Option<Capture>, String> {
            if let Some(capture) = self.events.front() {
                if !paused || capture.timestamp()? < discard_before_ns {
                    return Ok(self.events.pop_front());
                }
            }
            Ok(None)
        }
    }

    fn parse_control(contents: &str) -> Result<ControlCommand, String> {
        let mut fields = BTreeMap::<String, String>::new();
        for token in contents.split_whitespace() {
            let token = token.trim_matches('"');
            if let Some((key, value)) = token.split_once('=') {
                fields.insert(key.into(), value.into());
            }
        }
        let value = |key: &str| {
            fields
                .get(key)
                .ok_or_else(|| format!("control command lacks {key}"))
        };
        if value("v")? != "1" {
            return Err("unsupported journal control version".into());
        }
        let number = |key: &str| {
            value(key)?
                .parse::<u32>()
                .map_err(|_| format!("invalid {key} in control command"))
        };
        let state = match value("state")?.as_str() {
            "START" => ControlState::Started,
            "END" => ControlState::Ended,
            "PAUSE" => ControlState::PausePrepare,
            "PAUSE_COMMIT" => ControlState::PauseCommit,
            "RESUME" => ControlState::Resumed,
            _ => return Err("invalid journal control state".into()),
        };
        let requested_frame = number("frame")?;
        if requested_frame == 0 || requested_frame > LAST_FRAME {
            return Err("control frame is outside the supported range".into());
        }
        Ok(ControlCommand {
            build: value("build")?.clone(),
            epoch: number("epoch")?,
            slot: number("slot")?,
            sequence: number("sequence")?,
            state,
            requested_frame,
        })
    }

    #[test]
    fn lifecycle_follows_only_fresh_complete_ready_and_new_epochs() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target")
            .join(format!("journal-lifecycle-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("smashcraft-journal-ready-test-e1-p0.txt");
        let prefix = "function PreloadFiles takes nothing returns nothing\ncall Preload( \"SMASHCRAFT JOURNAL v=1 build=test epoch=1 slot=0 delay=0 first_frame=1\" )\n";
        fs::write(&path, prefix).unwrap();
        assert!(fresh_ready(&dir, "test", 0, 0, 0).unwrap().is_none());
        fs::write(&path, format!("{prefix}endfunction\n")).unwrap();
        let (epoch, delay, modified) = fresh_ready(&dir, "test", 0, 0, 0).unwrap().unwrap();
        assert_eq!((epoch, delay), (1, 0));
        assert!(fresh_ready(&dir, "test", 0, 0, modified).unwrap().is_none());
        assert!(fresh_ready(&dir, "test", 0, 1, 0).unwrap().is_none());
        assert!(fresh_ready(&dir, "test", 1, 0, 0).unwrap().is_none());
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn lifecycle_start_uses_publication_and_terminal_marker_follows_old_rows() {
        let prefix = "function PreloadFiles takes nothing returns nothing\ncall Preload( \"SMASHCRAFT JOURNAL CONTROL v=1 build=test epoch=1 slot=0 sequence=0 state=START frame=1\" )\n";
        assert!(control_if_complete(prefix).unwrap().is_none());
        let complete = format!("{prefix}endfunction\n");
        assert_eq!(control_if_complete(&complete).unwrap().unwrap().state, ControlState::Started);
        assert_eq!(parse_control(&complete.replace("START", "END")).unwrap().state, ControlState::Ended);
        let mut queue = PendingOutput::default();
        queue.push("I4old".into()).unwrap();
        queue.push("JE11".into()).unwrap();
        let mut window = TextWindow::default();
        let now = Instant::now();
        assert!(window.next(&queue, 1, now).unwrap().unwrap().1.contains("I4old"));
        window.sent(1, now);
        assert!(window.next(&queue, 1, now).unwrap().unwrap().1.contains("JE11"));
        window.sent(2, now);
        window.receipt(&mut queue, 2, 1, 1).unwrap();
        assert!(!queue.is_empty());
        window.receipt(&mut queue, 2, 2, 2).unwrap();
        assert!(queue.is_empty());
    }

    #[test]
    fn reads_native_preload_readiness() {
        let receipt = "function PreloadFiles takes nothing returns nothing\n\tcall Preload( \"SMASHCRAFT JOURNAL v=1 build=netcode-0022 epoch=1 slot=3\" )\n\tcall Preload( \"input=shadow-d0-r24 delay=0 rollback=24 first_frame=1\" )\nendfunction\n";
        assert_eq!(
            parse_ready(receipt).unwrap(),
            ("netcode-0022".into(), 1, 3, 0)
        );
        assert!(parse_ready(&receipt.replace("first_frame=1", "first_frame=2")).is_err());
    }

    #[test]
    fn reads_sequenced_pause_and_resume_control_receipts() {
        let pause = "function PreloadFiles takes nothing returns nothing\ncall Preload( \"SMASHCRAFT JOURNAL CONTROL v=1 build=playable epoch=7 slot=2 sequence=3 state=PAUSE frame=91\" )\nendfunction";
        assert_eq!(
            parse_control(pause).unwrap(),
            ControlCommand {
                build: "playable".into(),
                epoch: 7,
                slot: 2,
                sequence: 3,
                state: ControlState::PausePrepare,
                requested_frame: 91,
            }
        );
        let resume = pause.replace("sequence=3 state=PAUSE", "sequence=4 state=RESUME");
        assert_eq!(parse_control(&resume).unwrap().state, ControlState::Resumed);
        assert!(parse_control(&pause.replace("frame=91", "frame=0")).is_err());
    }

    #[test]
    fn control_reader_waits_for_the_native_writer_to_finish() {
        let command = "function PreloadFiles takes nothing returns nothing\ncall Preload( \"SMASHCRAFT JOURNAL CONTROL v=1 build=playable epoch=7 slot=2 sequence=3 state=PAUSE frame=91\" )\nendfunction\n";
        for length in 0..command.find("endfunction").unwrap() + "endfunction".len() {
            assert_eq!(control_if_complete(&command[..length]).unwrap(), None);
        }
        assert_eq!(
            control_if_complete(command).unwrap(),
            Some(parse_control(command).unwrap())
        );
        assert!(control_if_complete(&command.replace("v=1", "v=2")).is_err());
    }

    #[test]
    fn control_reader_ignores_an_earlier_game_reusing_the_same_identity() {
        let dir = env::temp_dir().join(format!("control-freshness-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = control_path(&dir, "test", 1, 0, 1);
        let clock = ControlClock::new(&dir).unwrap();
        let ready_ns = monotonic_ns().unwrap() - 2_000_000_000;
        let prefix = "function PreloadFiles takes nothing returns nothing\ncall Preload( \"SMASHCRAFT JOURNAL CONTROL v=1 build=test epoch=1 slot=0 sequence=1 state=PAUSE frame=1205\" )\n";
        fs::write(&path, format!("{prefix}endfunction\n")).unwrap();
        let old_time = UNIX_EPOCH + Duration::from_nanos((ready_ns as i128 + clock.offset_ns - 1_000_000_000) as u64);
        fs::File::open(&path).unwrap().set_times(fs::FileTimes::new().set_modified(old_time)).unwrap();
        let old = read_control(&path, clock, ready_ns).unwrap();
        assert!(old.publication.is_none(), "the earlier game's PAUSE must not pause frame 1");
        assert!(old.discard_before_ns >= ready_ns);

        let mut queue = PendingInput::default();
        let down = timed_button(ready_ns + 500_000_000, true);
        queue.push(down, false, false).unwrap();
        let Some(Capture::Event(event, ..)) = queue.pop_for_output(true, false, old.discard_before_ns).unwrap() else {
            panic!("current match input must still drain at its original timestamp");
        };
        assert_eq!(event, down);

        fs::write(&path, prefix).unwrap();
        let incomplete = read_control(&path, clock, ready_ns).unwrap();
        assert!(incomplete.publication.is_none());
        assert_eq!(incomplete.discard_before_ns, 0);
        OpenOptions::new().append(true).open(&path).unwrap().write_all(b"endfunction\n").unwrap();
        let current = read_control(&path, clock, ready_ns).unwrap().publication.unwrap();
        assert!(current.epoch_ns > ready_ns);
        assert_eq!(current.command, parse_control(&format!("{prefix}endfunction\n")).unwrap());
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn keyboard_mailbox_preserves_ascii_chunks_and_uses_nonconflicting_vks() {
        let mut codes = BTreeSet::new();
        for signal in 0..MAILBOX_SIGNAL_COUNT {
            let vk = mailbox_virtual_key(signal);
            assert!(codes.insert(vk), "duplicate carrier VK {vk:#x}");
            assert!(
                ![
                    0x1B, 0x09, 0x0D, 0x59, 0x70, 0x74, 0x79, 0x10, 0x11, 0x12, 0x5B, 0x5C
                ]
                .contains(&vk)
            );
        }
        for message in ["I40001a", "ACK1|4|P", "ACK1|4|PREPARE|19"] {
            for (index, chunk) in message.as_bytes().chunks(MAILBOX_CHUNK_BYTES).enumerate() {
                let final_chunk = (index + 1) * MAILBOX_CHUNK_BYTES >= message.len();
                let values = mailbox_signal_values(chunk, final_chunk, index % 2 == 0).unwrap();
                let decoded_length = (0..3)
                    .map(|bit| usize::from(values[49 + bit]) << bit)
                    .sum::<usize>();
                assert_eq!(decoded_length, chunk.len());
                let decoded = (0..decoded_length)
                    .map(|offset| {
                        let byte = (0..7)
                            .map(|bit| u8::from(values[offset * 7 + bit]) << bit)
                            .sum::<u8>();
                        char::from(byte)
                    })
                    .collect::<String>();
                assert_eq!(decoded.as_bytes(), chunk);
                assert_eq!(values[52], final_chunk);
                assert_eq!(values[53], index % 2 == 0);
            }
        }
    }

    #[test]
    fn keyboard_mailbox_ack_requires_complete_matching_identity() {
        let dir = std::env::temp_dir().join(format!("mailbox-ack-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = mailbox_ack_path(&dir, "test", 7, 2, 9);
        fs::write(&path, "function PreloadFiles takes nothing returns nothing\ncall Preload( \"SMASHCRAFT KEYBOARD ACK v=1 build=test epoch=7 slot=2 chunk=9\" )\n").unwrap();
        assert!(!mailbox_ack_matches(&path, "test", 7, 2, 9).unwrap());
        fs::write(&path, "function PreloadFiles takes nothing returns nothing\ncall Preload( \"SMASHCRAFT KEYBOARD ACK v=1 build=test epoch=7 slot=2 chunk=9\" )\nendfunction\n").unwrap();
        assert!(mailbox_ack_matches(&path, "test", 7, 2, 9).unwrap());
        assert!(!mailbox_ack_matches(&path, "test", 7, 2, 10).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn resumed_segment_excludes_the_entire_pause_gap() {
        let initial = FrameSegment {
            epoch_ns: 1_000_000_000,
            first_frame: 1,
        };
        let before_pause = frame_at(2_000_000_000, initial).unwrap();
        assert_eq!(before_pause, 61);
        let resumed = FrameSegment {
            epoch_ns: 302_000_000_000,
            first_frame: 61,
        };
        assert_eq!(frame_at(302_250_000_000, resumed).unwrap(), 76);
        assert_eq!(frame_at(302_000_000_000, resumed).unwrap(), before_pause);
    }

    #[test]
    fn pause_seal_stops_at_a_fixed_frontier_while_output_catches_up() {
        let segment = FrameSegment {
            epoch_ns: 1_000_000_000,
            first_frame: 1,
        };
        // The observed helper emitted frame 1159 while its live frontier had
        // reached frame 1366. The request timestamp gives a finite seal point;
        // output can drain to it without chasing the advancing clock.
        let published_ns = segment.epoch_ns + (1_200u128 * 1_000_000_000 / HZ);
        let seal = pause_seal_frontier(segment, published_ns, 1_205, 1_199);
        assert_eq!(seal, 1_204);
        let later_live_frontier =
            frame_through(segment, published_ns + (167u128 * 1_000_000_000 / HZ));
        assert_eq!(later_live_frontier, 1_366);
        assert!(seal < later_live_frontier);
        assert_eq!(pause_seal_frontier(segment, published_ns, 1_205, 1_199), seal);
        assert_eq!(pause_seal_frontier(segment, published_ns, 1_205, 1_210), 1_210);
    }

    #[test]
    fn pause_seal_keeps_post_request_events_queued() {
        let published_ns = 20_000_000_000;
        let mut input = PendingInput::default();
        let before = timed_button(published_ns - 1, true);
        let after = timed_button(published_ns + 1, false);
        input.push(before, false, true).unwrap();
        input.push(after, true, false).unwrap();

        let Capture::Event(drained, ..) = input.pop(true, published_ns).unwrap().unwrap() else {
            panic!("pre-publication input must be drained into its original frame");
        };
        assert_eq!(drained, before);
        assert!(input.pop(true, published_ns).unwrap().is_none());

        let Capture::Event(retained, ..) = input.pop(false, 0).unwrap().unwrap() else {
            panic!("post-publication input must remain available after sealing");
        };
        assert_eq!(retained, after);
    }

    #[test]
    fn queued_short_press_keeps_its_original_frames_across_a_250ms_service_stall() {
        let segment = FrameSegment {
            epoch_ns: 10_000_000_000,
            first_frame: 1,
        };
        let button_down_ns = 10_015_000_000;
        let button_up_ns = 10_035_000_000;
        // Both kernel events are processed together after the 250ms delay;
        // frame assignment still uses the original event timestamps.
        let delayed_service_now_ns = 10_285_000_000;
        assert!(delayed_service_now_ns - button_up_ns >= 250_000_000);
        assert_eq!(frame_at(button_down_ns, segment).unwrap(), 1);
        assert_eq!(frame_at(button_up_ns, segment).unwrap(), 3);
    }

    #[test]
    fn resume_starts_neutral_and_never_replays_held_pause_state() {
        let paused_state = State {
            sources: 1 << 0,
            ..State::default()
        };
        assert_ne!(action_state(paused_state), 0);
        let resumed_state = State::default();
        assert_eq!(action_state(resumed_state), 0);
        assert_eq!(
            encode_row(resumed_state, 0, Edges::default()),
            compact(0, 1)
        );
    }

    #[cfg(test)]
    fn timed_button(ns: u128, down: bool) -> evdev::InputEvent {
        libc::input_event {
            time: libc::timeval { tv_sec: (ns / 1_000_000_000) as _, tv_usec: ((ns % 1_000_000_000) / 1000) as _ },
            type_: evdev::EventType::KEY.0,
            code: Key::BTN_SOUTH.0,
            value: i32::from(down),
        }.into()
    }

    #[test]
    fn resume_publication_precedes_events_and_delayed_control_dequeue() {
        let dir = env::temp_dir().join(format!("resume-clock-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("resume.txt");
        let clock = ControlClock::new(&dir).unwrap();
        let mut queue = PendingInput::default();
        let mut input = FocusInput {
            eligible: true, armed: true, gameplay_armed: false,
            physical: State::default(), accept_since_ns: 0,
        };
        // A held paused button and an incompletely published command both
        // precede a genuine new tap; no intermediate drain may discard it.
        let held = timed_button(monotonic_ns().unwrap(), true);
        queue.push(held, false, false).unwrap();
        let prefix = "function PreloadFiles takes nothing returns nothing\ncall Preload( \"SMASHCRAFT JOURNAL CONTROL v=1 build=test epoch=1 slot=0 sequence=3 state=RESUME frame=91\" )\n";
        fs::write(&path, prefix).unwrap();
        let incomplete = read_control(&path, clock, 0).unwrap();
        assert!(incomplete.publication.is_none());
        assert!(queue.pop(true, incomplete.discard_before_ns).unwrap().is_none());
        thread::sleep(Duration::from_millis(5));
        OpenOptions::new().append(true).open(&path).unwrap().write_all(b"endfunction\n").unwrap();
        let original = read_control(&path, clock, 0).unwrap().publication.unwrap();
        thread::sleep(Duration::from_millis(20));
        queue.push(timed_button(monotonic_ns().unwrap(), false), false, false).unwrap();
        thread::sleep(Duration::from_millis(20));
        let down = timed_button(monotonic_ns().unwrap(), true);
        queue.push(down, false, false).unwrap();
        thread::sleep(Duration::from_millis(5));
        let up = timed_button(monotonic_ns().unwrap(), false);
        queue.push(up, false, false).unwrap();
        // The already-drained events remain queued while the helper cannot
        // service the completed command.
        thread::sleep(Duration::from_millis(80));
        let delayed = read_control(&path, clock, 0).unwrap().publication.unwrap();
        assert_eq!(original.epoch_ns, delayed.epoch_ns);
        assert!(delayed.read_ns - delayed.epoch_ns >= 100_000_000);
        assert!(delayed.uncertainty_ns < 10_000_000);
        let segment = FrameSegment { epoch_ns: delayed.epoch_ns, first_frame: 91 };
        let mut state = State::default();
        let mut edges = BTreeMap::new();
        let mut snapshots = BTreeMap::new();
        let mut accepted = Vec::new();
        while let Some(capture) = queue.pop(false, 0).unwrap() {
            let Capture::Event(event, before, after) = capture else { panic!("unexpected recovery boundary") };
            if input.accepts_in_segment(&[None; 6], event, before, after, true, segment).unwrap() {
                accepted.push(event);
                apply_event(&[None; 6], &mut state, event, &mut edges, &mut snapshots, 91, segment, false).unwrap();
            }
        }
        assert_eq!(accepted, vec![down, up]);
        let press_frame = frame_at(event_ns(&down).unwrap(), segment).unwrap();
        let release_frame = frame_at(event_ns(&up).unwrap(), segment).unwrap();
        assert_ne!(edges[&press_frame].pressed & ATTACK, 0);
        assert_ne!(edges[&release_frame].released & ATTACK, 0);
        assert_eq!(action_state(state), 0);
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn resume_clock_rejects_steps_and_future_publications() {
        let clock = ControlClock { offset_ns: 1_000_000_000, uncertainty_ns: 20, timestamp_resolution_ns: 1_000_000 };
        assert!(clock.validate(ControlClock { offset_ns: clock.offset_ns + 2_000_000, ..clock }).is_err());
        assert!(clock.publication(1_000_000_101, 100).is_err());
        assert_eq!(clock.publication(1_000_000_100, 100).unwrap(), 100);
    }

    #[test]
    fn resume_boundary_rearms_neutral_before_first_press_and_uses_half_open_frames() {
        let segment = FrameSegment { epoch_ns: 1_000_000_000, first_frame: 91 };
        assert!(frame_at(segment.epoch_ns - 1, segment).is_err());
        assert_eq!(frame_at(segment.epoch_ns, segment).unwrap(), 91);
        assert_eq!(frame_at(segment.epoch_ns + 16_666_666, segment).unwrap(), 91);
        assert_eq!(frame_at(segment.epoch_ns + 16_666_667, segment).unwrap(), 92);
        let mut input = FocusInput {
            eligible: true, armed: true, gameplay_armed: false,
            physical: State::default(), accept_since_ns: 0,
        };
        assert!(input.accepts_in_segment(&[None; 6], timed_button(segment.epoch_ns, true), false, false, true, segment).unwrap());
        assert_eq!(input.accept_since_ns, segment.epoch_ns);
    }

    fn monotonic_ns() -> io::Result<u128> {
        clock_ns(libc::CLOCK_MONOTONIC)
    }

    fn clock_ns(clock: libc::clockid_t) -> io::Result<u128> {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: ts is writable; callers supply Linux realtime or monotonic clock IDs.
        if unsafe { libc::clock_gettime(clock, &mut ts) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(ts.tv_sec as u128 * 1_000_000_000 + ts.tv_nsec as u128)
    }

    fn set_monotonic_event_clock(device: &RawDevice) -> io::Result<()> {
        let clock = libc::CLOCK_MONOTONIC;
        // SAFETY: ioctl receives the live evdev fd and a pointer to one initialized clock id.
        let result = unsafe { libc::ioctl(device.as_raw_fd(), EVIOCSCLOCKID, &clock) };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn set_nonblocking(device: &RawDevice) -> io::Result<()> {
        // SAFETY: fcntl reads and updates flags on the live evdev descriptor.
        let flags = unsafe { libc::fcntl(device.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: F_SETFL accepts the original descriptor flags plus O_NONBLOCK.
        let result =
            unsafe { libc::fcntl(device.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn compact(mut value: u32, width: usize) -> String {
        let mut bytes = vec![b'0'; width];
        for i in (0..width).rev() {
            bytes[i] = ALPHABET[(value % 64) as usize];
            value /= 64;
        }
        String::from_utf8(bytes).expect("base64 alphabet is UTF-8")
    }

    fn signed(value: i16) -> u32 {
        if value < 0 {
            (-2 * i32::from(value) - 1) as u32
        } else {
            2 * value as u32
        }
    }

    fn action_state(s: State) -> u32 {
        let mut held = 0;
        if s.sources & (1 << 0) != 0 {
            held |= ATTACK;
        }
        if s.sources & (1 << 1) != 0 || s.sources & (1 << 3) != 0 {
            held |= JUMP;
        }
        if s.sources & (1 << 2) != 0 {
            held |= SPECIAL;
        }
        if s.sources & (1 << 4) != 0 {
            held |= WALK;
        }
        if s.sources & (1 << 5) != 0 {
            held |= GRAB;
        }
        let (x, y) = melee_stick(s.x, s.y);
        if x < 0 {
            held |= MOVE_LEFT;
        }
        if x > 0 {
            held |= MOVE_RIGHT;
        }
        if stick_down(y) {
            held |= MOVE_DOWN;
        }
        if y < 0 {
            held |= MOVE_UP;
        }
        let (cx, cy) = melee_stick(s.cx, s.cy);
        if cx < -11_000 {
            held |= SMASH_LEFT;
        }
        if cx > 11_000 {
            held |= SMASH_RIGHT;
        }
        if cy < -11_000 {
            held |= SMASH_UP;
        }
        if cy > 11_000 {
            held |= SMASH_DOWN;
        }
        if s.lt > 4_000 {
            held |= LEFT_TRIGGER;
        }
        if s.rt > 4_000 {
            held |= RIGHT_TRIGGER;
        }
        held
    }

    fn axis_byte(raw: i16) -> i16 {
        (i32::from(raw) * 127 / 32_767).clamp(-127, 127) as i16
    }

    fn encode_row(state: State, previous: u32, edges: Edges) -> String {
        let held = action_state(state);
        let pressed = edges.pressed | (held & !previous);
        let released = edges.released | (previous & !held);
        let mut flags = 0u32;
        let mut body = String::new();
        if held != 0 {
            flags |= 1;
            body.push_str(&compact(held, 3));
        }
        if pressed != 0 || released != 0 {
            flags |= 2;
            body.push_str(&compact(pressed, 3));
            body.push_str(&compact(released, 3));
        }
        let (x, y) = melee_stick(state.x, state.y);
        let x = axis_byte(x);
        let y = axis_byte(-y);
        let axes = signed(x) * 255 + signed(y);
        if axes != 0 {
            flags |= 4;
            body.push_str(&compact(axes, 3));
        }
        let lt = u32::from(state.lt.min(32_767)) * 255 / 32_767;
        let rt = u32::from(state.rt.min(32_767)) * 255 / 32_767;
        let triggers = lt * 256 + rt;
        if triggers != 0 {
            flags |= 8;
            body.push_str(&compact(triggers, 3));
        }
        let metadata = signed(edges.special_x)
            + 3 * signed(edges.special_y)
            + 9 * signed(edges.dodge_x)
            + 27 * signed(edges.dodge_y)
            + 81 * signed(edges.sdi_x)
            + 243 * signed(edges.sdi_y)
            + 729 * signed(edges.ledge);
        if metadata != 0 {
            flags |= 16;
            body.push_str(&compact(metadata, 2));
        }
        let throws = signed(edges.throw_x) * 255 + signed(edges.throw_y);
        if throws != 0 {
            flags |= 32;
            body.push_str(&compact(throws, 3));
        }
        compact(flags, 1) + &body
    }

    fn encode_packet(epoch: u32, first: u32, rows: &[String]) -> String {
        let mut wire = format!("I4{}", compact(rows.len() as u32, 1));
        encode_counter(epoch, &mut wire);
        encode_counter(first, &mut wire);
        for row in rows {
            wire.push_str(row);
        }
        wire
    }

    fn encode_counter(mut value: u32, wire: &mut String) {
        loop {
            let digit = value & 31;
            value >>= 5;
            wire.push(ALPHABET[(digit | if value == 0 { 0 } else { 32 }) as usize] as char);
            if value == 0 {
                return;
            }
        }
    }

    fn event_ns(event: &evdev::InputEvent) -> Result<u128, String> {
        event
            .timestamp()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .map_err(|_| "negative evdev CLOCK_MONOTONIC timestamp".into())
    }

    fn frame_at(t: u128, segment: FrameSegment) -> Result<u32, String> {
        if t < segment.epoch_ns {
            return Err(format!(
                "event at {t} predates declared capture segment {}",
                segment.epoch_ns
            ));
        }
        let f = segment.first_frame as u128 + ((t - segment.epoch_ns) * HZ / 1_000_000_000);
        if f > LAST_FRAME as u128 {
            return Err("frame counter exhausted".into());
        }
        Ok(f as u32)
    }

    fn normalized_axis(ranges: &[Option<evdev::AbsInfo>; 6], code: Abs, value: i32) -> i16 {
        let Some(info) = ranges.get(code.0 as usize).copied().flatten() else {
            return 0;
        };
        let lo = info.minimum();
        let hi = info.maximum();
        if hi <= lo {
            return 0;
        }
        let mid = (lo + hi) / 2;
        if value >= mid {
            ((value - mid) * 32_767 / (hi - mid).max(1)).clamp(0, 32_767) as i16
        } else {
            ((value - mid) * 32_768 / (mid - lo).max(1)).clamp(-32_768, 0) as i16
        }
    }

    fn normalized_trigger(ranges: &[Option<evdev::AbsInfo>; 6], code: Abs, value: i32) -> u16 {
        let Some(info) = ranges.get(code.0 as usize).copied().flatten() else {
            return 0;
        };
        let range = info.maximum() - info.minimum();
        if range <= 0 {
            return 0;
        }
        ((value - info.minimum()).clamp(0, range) * 32_767 / range) as u16
    }

    /// View held this long asks the map to save the last seconds of play.
    const MOMENT_HOLD_NS: u128 = 1_000_000_000;

    /// View (Back) held for a second: one request per hold to save a moment.
    #[derive(Default)]
    struct MomentHold {
        since_ns: Option<u128>,
        sent: bool,
    }

    impl MomentHold {
        fn observe(&mut self, event: &evdev::InputEvent, event_ns: u128) {
            let EventSummary::Key(_, Key::BTN_SELECT, value) = event.destructure() else {
                return;
            };
            if value == 0 {
                self.since_ns = None;
                self.sent = false;
            } else if value == 1 && self.since_ns.is_none() {
                self.since_ns = Some(event_ns);
            }
        }

        /// The map's moment request ("JM1" and the epoch in ten digits) once View has been held a second.
        fn request(&mut self, now_ns: u128, epoch: u32) -> Option<String> {
            let since = self.since_ns?;
            if self.sent || now_ns.saturating_sub(since) < MOMENT_HOLD_NS {
                return None;
            }
            self.sent = true;
            Some(format!("JM1{epoch:010}"))
        }
    }

    #[test]
    fn view_held_a_second_requests_a_moment_once_per_hold() {
        let down = evdev::InputEvent::new(evdev::EventType::KEY.0, Key::BTN_SELECT.0, 1);
        let up = evdev::InputEvent::new(evdev::EventType::KEY.0, Key::BTN_SELECT.0, 0);
        let mut hold = MomentHold::default();
        hold.observe(&down, 5_000_000_000);
        assert_eq!(hold.request(5_999_999_999, 7), None);
        assert_eq!(hold.request(6_000_000_000, 7).as_deref(), Some("JM10000000007"));
        assert_eq!(hold.request(9_000_000_000, 7), None);
        hold.observe(&up, 9_100_000_000);
        assert_eq!(hold.request(10_200_000_000, 7), None);
        hold.observe(&down, 10_000_000_000);
        assert_eq!(hold.request(11_000_000_000, 8).as_deref(), Some("JM10000000008"));
    }

    fn pause_request_wire(
        event: &evdev::InputEvent,
        start_held: &mut bool,
        epoch: u32,
        sequence: u32,
        paused: bool,
        pending: bool,
    ) -> Option<String> {
        let EventSummary::Key(_, Key::BTN_START, value) = event.destructure() else {
            return None;
        };
        let pressed = value == 1 && !*start_held;
        if value != 2 {
            *start_held = value != 0;
        }
        if !pressed || pending {
            return None;
        }
        Some(format!(
            "JP1{epoch:010}{sequence:010}{}",
            if paused { 'R' } else { 'P' }
        ))
    }

    #[test]
    fn controller_start_requests_pause_and_resume_once_per_press() {
        let down = evdev::InputEvent::new(evdev::EventType::KEY.0, Key::BTN_START.0, 1);
        let repeat = evdev::InputEvent::new(evdev::EventType::KEY.0, Key::BTN_START.0, 2);
        let up = evdev::InputEvent::new(evdev::EventType::KEY.0, Key::BTN_START.0, 0);
        let mut held = false;
        assert_eq!(
            pause_request_wire(&down, &mut held, 7, 1, false, false).as_deref(),
            Some("JP100000000070000000001P")
        );
        assert_eq!(
            pause_request_wire(&repeat, &mut held, 7, 1, false, false),
            None
        );
        assert_eq!(
            pause_request_wire(&down, &mut held, 7, 3, true, false),
            None
        );
        assert_eq!(pause_request_wire(&up, &mut held, 7, 3, true, false), None);
        assert_eq!(
            pause_request_wire(&down, &mut held, 7, 3, true, false).as_deref(),
            Some("JP100000000070000000003R")
        );
        pause_request_wire(&up, &mut held, 7, 3, true, true);
        assert_eq!(pause_request_wire(&down, &mut held, 7, 3, true, true), None);
        assert_eq!(
            pause_request_wire(&down, &mut held, 7, 3, true, false),
            None
        );
    }

    fn update_state(
        ranges: &[Option<evdev::AbsInfo>; 6],
        state: &mut State,
        event: evdev::InputEvent,
    ) {
        let summary = event.destructure();
        match summary {
            EventSummary::Key(_, key, value) => {
                let bit = match key {
                    Key::BTN_SOUTH => Some(1 << 0),
                    Key::BTN_EAST => Some(1 << 1),
                    Key::BTN_WEST => Some(1 << 2),
                    Key::BTN_NORTH => Some(1 << 3),
                    Key::BTN_TL => Some(1 << 4),
                    Key::BTN_TR => Some(1 << 5),
                    _ => None,
                };
                if let Some(bit) = bit {
                    if value == 1 {
                        state.sources |= bit;
                    } else if value == 0 {
                        state.sources &= !bit;
                    }
                }
            }
            EventSummary::AbsoluteAxis(_, code, value) => match code {
                Abs::ABS_X => state.x = normalized_axis(ranges, code, value),
                Abs::ABS_Y => state.y = normalized_axis(ranges, code, value),
                Abs::ABS_RX => state.cx = normalized_axis(ranges, code, value),
                Abs::ABS_RY => state.cy = normalized_axis(ranges, code, value),
                Abs::ABS_Z => state.lt = normalized_trigger(ranges, code, value),
                Abs::ABS_RZ => state.rt = normalized_trigger(ranges, code, value),
                _ => {}
            },
            _ => {}
        }
    }

    #[test]
    fn journal_jump_sources_retain_hold_until_last_release() {
        let ranges = [None; 6];
        let sources = [
            (evdev::EventType::KEY.0, Key::BTN_EAST.0, 1),
            (evdev::EventType::KEY.0, Key::BTN_NORTH.0, 1),
        ];
        for first in 0..sources.len() {
            for second in 0..sources.len() {
                if first == second {
                    continue;
                }
                let mut state = State::default();
                let mut edges = BTreeMap::new();
                let mut snapshots = BTreeMap::new();
                let segment = FrameSegment { epoch_ns: 0, first_frame: 1 };
                for (step, (source, down)) in [(first, true), (second, true), (first, false), (second, false)].into_iter().enumerate() {
                    let (kind, code, pressed_value) = sources[source];
                    let ns = 20_000_000 * (step as u128 + 1);
                    let event = libc::input_event {
                        time: libc::timeval {
                            tv_sec: 0,
                            tv_usec: (ns / 1000) as _,
                        },
                        type_: kind,
                        code,
                        value: if down { pressed_value } else { 0 },
                    }.into();
                    apply_event(&ranges, &mut state, event, &mut edges, &mut snapshots, 1, segment, false).unwrap();
                    let frame = frame_at(ns, segment).unwrap();
                    assert_eq!(action_state(snapshots[&frame]) & JUMP != 0, step < 3,
                        "sources {first}/{second}, step {step}");
                    assert_eq!(edges[&frame].pressed & JUMP != 0, step == 0,
                        "extra jump press for sources {first}/{second}, step {step}");
                    assert_eq!(edges[&frame].released & JUMP != 0, step == 3,
                        "premature jump release for sources {first}/{second}, step {step}");
                }
                assert_eq!(action_state(state), 0);
            }
        }
    }

    #[test]
    fn stick_up_is_only_up_and_never_jumps() {
        let mut ranges = [None; 6];
        ranges[Abs::ABS_Y.0 as usize] = Some(evdev::AbsInfo::new(0, -32_768, 32_767, 16, 128, 0));
        let mut state = State::default();
        let mut edges = BTreeMap::new();
        let mut snapshots = BTreeMap::new();
        let segment = FrameSegment { epoch_ns: 0, first_frame: 1 };
        for (step, value) in [-32_768, -20_000, 0].into_iter().enumerate() {
            let ns = 20_000_000 * (step as u128 + 1);
            let event = libc::input_event {
                time: libc::timeval { tv_sec: 0, tv_usec: (ns / 1000) as _ },
                type_: evdev::EventType::ABSOLUTE.0,
                code: Abs::ABS_Y.0,
                value,
            }.into();
            apply_event(&ranges, &mut state, event, &mut edges, &mut snapshots, 1, segment, false).unwrap();
            let frame = frame_at(ns, segment).unwrap();
            assert_eq!(action_state(snapshots[&frame]), if value < 0 { MOVE_UP } else { 0 }, "step {step}");
            assert_eq!((edges[&frame].pressed | edges[&frame].released) & JUMP, 0, "step {step}");
        }
        assert_eq!(edges[&frame_at(20_000_000, segment).unwrap()].pressed, MOVE_UP);
    }

    #[test]
    fn stick_down_is_reported_only_at_melees_strong_threshold() {
        let mut ranges = [None; 6];
        ranges[Abs::ABS_Y.0 as usize] = Some(evdev::AbsInfo::new(0, -32_768, 32_767, 16, 128, 0));
        let mut state = State::default();
        let mut edges = BTreeMap::new();
        let mut snapshots = BTreeMap::new();
        let segment = FrameSegment { epoch_ns: 0, first_frame: 1 };
        // Positive evdev y is down: 16384 is half, 22937 is 0.7, 9000 is inside the deadzone.
        let steps = [(16_384, 0), (22_937, MOVE_DOWN), (16_384, 0), (32_767, MOVE_DOWN), (9_000, 0)];
        for (step, (value, held)) in steps.into_iter().enumerate() {
            let ns = 20_000_000 * (step as u128 + 1);
            let event = libc::input_event {
                time: libc::timeval { tv_sec: 0, tv_usec: (ns / 1000) as _ },
                type_: evdev::EventType::ABSOLUTE.0,
                code: Abs::ABS_Y.0,
                value,
            }.into();
            apply_event(&ranges, &mut state, event, &mut edges, &mut snapshots, 1, segment, false).unwrap();
            let frame = frame_at(ns, segment).unwrap();
            assert_eq!(action_state(snapshots[&frame]), held, "step {step}");
            let edge = edges.get(&frame).map_or((0, 0), |e| (e.pressed, e.released));
            let expected = match step { 1 | 3 => (MOVE_DOWN, 0), 2 | 4 => (0, MOVE_DOWN), _ => (0, 0) };
            assert_eq!(edge, expected, "step {step}");
        }
        assert_eq!(edges[&frame_at(40_000_000, segment).unwrap()].ledge, -1);
    }

    /// How long focus may be elsewhere before captured input disarms. Focus
    /// checks have reported the game unfocused for a few milliseconds at a
    /// time during play; 200 ms (12 frames) is over 30 times the observed
    /// blip yet shorter than a person switching windows and then pressing a
    /// pad button. Until it elapses, rows keep their held state and typing
    /// alone waits for focus.
    const FOCUS_GRACE_NS: u128 = 200_000_000;

    #[derive(Default)]
    struct FocusGrace {
        away_since: Option<u128>,
        /// A loss of at least the grace period that ended between two
        /// gameplay focus observations.
        lost: bool,
    }

    #[derive(Debug, PartialEq, Eq)]
    enum FocusChange {
        Away,
        Back { away_ns: u128, lost: bool },
    }

    impl FocusGrace {
        fn observe(&mut self, eligible: bool, now: u128) -> Option<FocusChange> {
            match (eligible, self.away_since) {
                (false, None) => {
                    self.away_since = Some(now);
                    Some(FocusChange::Away)
                }
                (true, Some(since)) => {
                    self.away_since = None;
                    let away_ns = now.saturating_sub(since);
                    let lost = away_ns >= FOCUS_GRACE_NS;
                    self.lost |= lost;
                    Some(FocusChange::Back { away_ns, lost })
                }
                _ => None,
            }
        }

        fn held(&self, now: u128) -> bool {
            self.away_since
                .is_none_or(|since| now.saturating_sub(since) < FOCUS_GRACE_NS)
        }
    }

    /// One main-loop focus observation: returns whether input stays armed and
    /// whether held controls must be released now.
    #[cfg(test)]
    fn observe_focus(grace: &mut FocusGrace, input: &mut FocusInput, eligible: bool, now: u128) -> (bool, bool) {
        grace.observe(eligible, now);
        let held = grace.held(now);
        let lost = std::mem::take(&mut grace.lost);
        (held, input.observe(held, lost, now))
    }

    #[cfg(test)]
    fn timed_stick_x(ns: u128, value: i32) -> evdev::InputEvent {
        libc::input_event {
            time: libc::timeval { tv_sec: (ns / 1_000_000_000) as _, tv_usec: ((ns % 1_000_000_000) / 1000) as _ },
            type_: evdev::EventType::ABSOLUTE.0,
            code: Abs::ABS_X.0,
            value,
        }.into()
    }

    #[test]
    fn ten_millisecond_focus_blip_keeps_input_armed_and_held_state() {
        let mut ranges = [None; 6];
        ranges[Abs::ABS_X.0 as usize] = Some(evdev::AbsInfo::new(0, -32_768, 32_767, 16, 128, 0));
        let forward = State { x: 32_767, sources: 1, ..State::default() };
        let mut input = FocusInput { eligible: true, armed: true, gameplay_armed: true, physical: forward, accept_since_ns: 0 };
        let mut grace = FocusGrace::default();
        let state = forward;
        let row = encode_row(state, action_state(state), Edges::default());
        let ms = 1_000_000;
        for t in 1_000..1_030 {
            let (armed, release) = observe_focus(&mut grace, &mut input, !(1_005..1_015).contains(&t), t * ms);
            assert!(armed && !release, "t={t} ms");
            assert!(input.armed && input.gameplay_armed, "t={t} ms");
        }
        assert_eq!(grace.observe(true, 1_030 * ms), None);
        // Input during the blip is captured, not dropped.
        assert!(input.accepts(&ranges, timed_stick_x(1_010 * ms, 32_767), false, true).unwrap());
        // Rows keep holding forward with no release and re-press, so the map
        // sees no new dash.
        assert_eq!(encode_row(state, action_state(state), Edges::default()), row);
        assert_ne!(action_state(state) & MOVE_RIGHT, 0);
    }

    #[test]
    fn half_second_focus_loss_releases_once_and_requires_neutral() {
        let mut ranges = [None; 6];
        ranges[Abs::ABS_X.0 as usize] = Some(evdev::AbsInfo::new(0, -32_768, 32_767, 16, 128, 0));
        let forward = State { x: 32_767, sources: 1, ..State::default() };
        let mut input = FocusInput { eligible: true, armed: true, gameplay_armed: true, physical: forward, accept_since_ns: 0 };
        let mut grace = FocusGrace::default();
        let mut state = forward;
        let mut edges = BTreeMap::new();
        let mut snapshots = BTreeMap::new();
        let segment = FrameSegment { epoch_ns: 0, first_frame: 1 };
        let ms = 1_000_000;
        let mut released = Vec::new();
        for t in 1_000..1_500 {
            let (_, release) = observe_focus(&mut grace, &mut input, false, t * ms);
            if release {
                released.push(t);
                let frame = release_for_focus_loss(&mut state, &mut edges, &mut snapshots, 1, segment, t * ms).unwrap();
                // The map receives a neutral row at the release frame.
                let row = encode_row(snapshots[&frame], action_state(forward), edges[&frame]);
                assert_eq!(&row[..1], "2", "release row holds nothing and has centred axes: {row}");
                assert_eq!(edges[&frame].released & (MOVE_RIGHT | ATTACK), MOVE_RIGHT | ATTACK);
            }
        }
        assert_eq!(released, [1_200]);
        assert_eq!(state, State::default());
        assert!(!input.armed);
        assert_eq!(grace.observe(true, 1_500 * ms), Some(FocusChange::Back { away_ns: 500 * ms, lost: true }));
        let (armed, release) = observe_focus(&mut grace, &mut input, true, 1_500 * ms);
        assert!(armed && !release);
        // The stick is still forward: nothing re-arms or resends it.
        assert!(!input.accepts(&ranges, timed_stick_x(1_510 * ms, 32_767), false, true).unwrap());
        input.rearm(1_515 * ms, false, true);
        assert!(!input.armed);
        assert_eq!(state, State::default());
        // Neutral re-arms; the next push is a genuine new press.
        assert!(!input.accepts(&ranges, timed_button(1_518 * ms, false), false, true).unwrap());
        assert!(!input.armed);
        assert!(!input.accepts(&ranges, timed_stick_x(1_520 * ms, 0), false, true).unwrap());
        assert!(input.armed);
        assert!(input.accepts(&ranges, timed_stick_x(1_530 * ms, 32_767), false, true).unwrap());
    }

    #[test]
    fn a_long_loss_between_gameplay_observations_still_releases() {
        let mut grace = FocusGrace::default();
        let mut input = FocusInput { eligible: true, armed: true, gameplay_armed: true, physical: State { sources: 1, ..State::default() }, accept_since_ns: 0 };
        // Typing saw the loss; the gameplay loop next looks after focus returned.
        grace.observe(false, 1_000_000_000);
        grace.observe(true, 1_300_000_000);
        assert_eq!(observe_focus(&mut grace, &mut input, true, 1_301_000_000), (true, true));
        assert!(!input.armed);
    }

    struct FocusInput {
        eligible: bool,
        armed: bool,
        gameplay_armed: bool,
        physical: State,
        accept_since_ns: u128,
    }

    impl FocusInput {
        fn restore(&mut self, physical: State, start: bool, ns: u128, gameplay: bool) {
            self.physical = physical;
            self.accept_since_ns = self.accept_since_ns.max(ns);
            self.armed = self.eligible && action_state(physical) == 0 && !start;
            self.gameplay_armed = self.armed && gameplay;
        }

        fn observe(&mut self, eligible: bool, lost: bool, now: u128) -> bool {
            let disarm = lost || !eligible;
            let release = disarm && self.armed;
            if disarm || self.eligible != eligible {
                self.armed = false;
                self.gameplay_armed = false;
                self.accept_since_ns = self.accept_since_ns.max(now);
            }
            self.eligible = eligible;
            release
        }

        fn accepts(
            &mut self,
            ranges: &[Option<evdev::AbsInfo>; 6],
            event: evdev::InputEvent,
            start_held: bool,
            gameplay: bool,
        ) -> Result<bool, String> {
            if matches!(
                event.destructure(),
                EventSummary::Synchronization(_, evdev::SynchronizationCode::SYN_DROPPED, _)
            ) {
                return Err("kernel reported SYN_DROPPED; lost original input edges cannot be reconstructed".into());
            }
            update_state(ranges, &mut self.physical, event);
            let current = event_ns(&event)? >= self.accept_since_ns;
            if !self.eligible {
                self.armed = false;
                self.gameplay_armed = false;
                return Ok(false);
            }
            if !self.armed {
                self.armed = current && action_state(self.physical) == 0 && !start_held;
                self.gameplay_armed = self.armed && gameplay;
                return Ok(false);
            }
            if !gameplay {
                self.gameplay_armed = false;
                return Ok(false);
            }
            if !self.gameplay_armed {
                self.gameplay_armed = current && action_state(self.physical) == 0 && !start_held;
                return Ok(false);
            }
            Ok(current)
        }

        fn accepts_in_segment(
            &mut self,
            ranges: &[Option<evdev::AbsInfo>; 6],
            event: evdev::InputEvent,
            start_before: bool,
            start_after: bool,
            capturing: bool,
            segment: FrameSegment,
        ) -> Result<bool, String> {
            let gameplay = capturing && event_ns(&event)? >= segment.epoch_ns;
            if gameplay {
                // Use physical state before this event and the publication
                // boundary. Dequeue time must not erase retained input.
                self.rearm(segment.epoch_ns, start_before, true);
            }
            self.accepts(ranges, event, start_after, gameplay)
        }

        fn rearm(&mut self, now: u128, start_held: bool, gameplay: bool) {
            if !gameplay {
                self.gameplay_armed = false;
            }
            if now >= self.accept_since_ns
                && self.eligible
                && (!self.armed || (gameplay && !self.gameplay_armed))
                && action_state(self.physical) == 0
                && !start_held
            {
                self.armed = true;
                self.gameplay_armed = gameplay;
                self.accept_since_ns = self.accept_since_ns.max(now);
            }
        }
    }

    #[test]
    fn focus_loss_retains_assigned_rows_and_releases_after_the_open_frame() {
        let mut state = State {
            sources: 1,
            ..State::default()
        };
        let mut edges = BTreeMap::from([(
            2,
            Edges {
                pressed: ATTACK,
                ..Edges::default()
            },
        )]);
        let mut snapshots = BTreeMap::from([(2, state)]);
        let segment = FrameSegment {
            epoch_ns: 0,
            first_frame: 1,
        };
        let frame = release_for_focus_loss(
            &mut state,
            &mut edges,
            &mut snapshots,
            2,
            segment,
            20_000_000,
        )
        .unwrap();
        assert_eq!(frame, 3);
        assert_eq!(edges[&2].pressed, ATTACK);
        assert_eq!(snapshots[&2].sources, 1);
        assert_eq!(edges[&3].released, ATTACK);
        assert_eq!(snapshots[&3], State::default());
        assert_eq!(state, State::default());
        assert_eq!(frame_at(100_000_000, segment).unwrap(), 7);
    }

    #[test]
    fn text_queue_retains_sent_records_until_native_consumption_and_bounds_backlog() {
        let mut queue = PendingOutput::default();
        let first = encode_packet(
            7,
            17,
            &[encode_row(
                State {
                    sources: 1,
                    ..State::default()
                },
                0,
                Edges::default(),
            )],
        );
        queue.push(first.clone()).unwrap();
        queue.push("ACK1|2|P|18".into()).unwrap();
        let mut window = TextWindow::default();
        let now = Instant::now();
        let pending = window.next(&queue, 7, now).unwrap().unwrap();
        assert_eq!(window.next(&queue, 7, now).unwrap().unwrap(), pending);
        assert_eq!(queue.records.front(), Some(&first));
        window.sent(pending.0, now);
        let second = window.next(&queue, 7, now).unwrap().unwrap();
        window.sent(second.0, now);
        assert_eq!(queue.records.len(), 2);
        assert!(window.next(&queue, 7, now).unwrap().is_none());
        window.suspend();
        window.receipt(&mut queue, 0, 0, 1).unwrap();
        assert_eq!(
            window.next(&queue, 7, now).unwrap().unwrap(),
            pending
        );
        window.sent(pending.0, now);
        assert!(window.next(&queue, 7, now).unwrap().is_none());
        window.acknowledge(&mut queue, 1).unwrap();
        assert_eq!(
            queue.records.front().map(String::as_str),
            Some("ACK1|2|P|18")
        );
        assert!(window.acknowledge(&mut queue, 3).is_err());
        window.acknowledge(&mut queue, 2).unwrap();
        assert!(queue.is_empty());
        assert_eq!(queue.bytes, 0);
        for _ in 0..OUTPUT_RECORD_LIMIT {
            queue.push("0".into()).unwrap();
        }
        assert!(queue.push("extra".into()).is_err());
        assert_eq!(queue.records.len(), OUTPUT_RECORD_LIMIT);
        let mut bytes = PendingOutput::default();
        bytes.push("a".repeat(OUTPUT_BYTE_LIMIT - 1)).unwrap();
        assert!(bytes.push("b".into()).is_err());
        assert_eq!(bytes.bytes, OUTPUT_BYTE_LIMIT);
    }

    #[test]
    fn output_backpressure_retains_original_edges_until_consumed_credit_returns() {
        let mut output = PendingOutput::default();
        // Dense input fills bytes first; neutral input fills record count first.
        for wire in ["I421100".to_owned(), "I4".to_owned() + &"0".repeat(57)] {
            while output.frame_credit() {
                output.push(wire.clone()).unwrap();
            }
            let retained_records = output.records.len();
            let retained_bytes = output.bytes;
            let mut input = PendingInput::default();
            let down = timed_button(20_000_000, true);
            let up = timed_button(25_000_000, false);
            input.push(down, false, false).unwrap();
            input.push(up, false, false).unwrap();
            assert!(input.pop_for_output(output.frame_credit(), false, 0).unwrap().is_none());
            assert_eq!(input.events.len(), 2);
            assert_eq!(output.records.len(), retained_records);
            assert_eq!(output.bytes, retained_bytes);
            let mut window = TextWindow::default();
            let now = Instant::now();
            for sequence in 1..=16 {
                assert_eq!(window.next(&output, 2, now).unwrap().unwrap().0, sequence);
                window.sent(sequence, now);
            }
            window.receipt(&mut output, 16, 16, 1).unwrap();
            assert!(output.frame_credit());
            // Only regained credit permits sealing the final singleton and ACK.
            output.push(encode_packet(2, 17, &["0".repeat(21)])).unwrap();
            output.push("ACK1|2147483647|P|2147483646".into()).unwrap();
            assert!(output.frame_credit());
            let segment = FrameSegment { epoch_ns: 0, first_frame: 1 };
            let mut state = State::default();
            let mut edges = BTreeMap::new();
            let mut snapshots = BTreeMap::new();
            while let Some(capture) = input.pop_for_output(output.frame_credit(), false, 0).unwrap() {
                let Capture::Event(event, ..) = capture else { panic!("unexpected boundary") };
                apply_event(&[None; 6], &mut state, event, &mut edges, &mut snapshots, 1, segment, false).unwrap();
            }
            assert!(input.events.is_empty());
            assert_eq!(edges.len(), 1);
            assert_eq!(edges[&2].pressed, ATTACK);
            assert_eq!(edges[&2].released, ATTACK);
            assert_eq!(action_state(state), 0);
            while output.pop().is_some() {}
        }
    }

    #[test]
    fn text_window_is_pipelined_and_retries_original_sequence_after_missing_receipt() {
        let mut queue = PendingOutput::default();
        for _ in 0..20 {
            queue.push("I421100".into()).unwrap();
        }
        let mut window = TextWindow::default();
        let now = Instant::now();
        let first = window.next(&queue, 1, now).unwrap().unwrap();
        for sequence in 1..=16 {
            let record = window.next(&queue, 1, now).unwrap().unwrap();
            assert_eq!(record.0, sequence);
            window.sent(sequence, now);
        }
        assert!(window.next(&queue, 1, now).unwrap().is_none());
        assert_eq!(queue.records.len(), 20);
        assert!(window.next(&queue, 1, now + TEXT_RETRY).unwrap().is_none());
        window.receipt(&mut queue, 0, 0, 1).unwrap();
        assert_eq!(
            window.next(&queue, 1, now + TEXT_RETRY).unwrap().unwrap(),
            first
        );
        window.sent(1, now + TEXT_RETRY);
        window.receipt(&mut queue, 16, 0, 2).unwrap();
        assert!(window.next(&queue, 1, now + TEXT_RETRY).unwrap().is_none());
        window.receipt(&mut queue, 16, 1, 3).unwrap();
        assert_eq!(
            window.next(&queue, 1, now + TEXT_RETRY).unwrap().unwrap().0,
            17
        );
        window.receipt(&mut queue, 16, 12, 4).unwrap();
        assert_eq!(queue.records.len(), 8);
    }

    #[test]
    fn stalled_native_receiver_cannot_accumulate_unbounded_retry_copies() {
        let mut queue = PendingOutput::default();
        for _ in 0..16 { queue.push("I421100".into()).unwrap(); }
        let mut window = TextWindow::default();
        let now = Instant::now();
        window.receipt(&mut queue, 0, 0, 1).unwrap();
        for sequence in 1..=16 {
            assert_eq!(window.next(&queue, 1, now).unwrap().unwrap().0, sequence);
            window.sent(sequence, now);
        }
        let retry = now + TEXT_RETRY;
        assert_eq!(window.next(&queue, 1, retry).unwrap().unwrap().0, 1);
        window.sent(1, retry);
        assert!(window.next(&queue, 1, retry).unwrap().is_none());
        for attempt in 2..=100 {
            window.receipt(&mut queue, 0, 0, 1).unwrap();
            assert!(window.next(&queue, 1, now + TEXT_RETRY * attempt).unwrap().is_none());
        }
        assert_eq!(queue.records.len(), 16);
        window.receipt(&mut queue, 0, 0, 2).unwrap();
        let fresh_retry = now + TEXT_RETRY * 101;
        assert_eq!(window.next(&queue, 1, fresh_retry).unwrap().unwrap().0, 1);
        window.sent(1, fresh_retry);
        assert!(window.next(&queue, 1, fresh_retry).unwrap().is_none());
        window.receipt(&mut queue, 16, 16, 3).unwrap();
        assert!(queue.is_empty());
        assert!(window.receipt(&mut queue, 16, 16, 4).is_ok());
    }

    #[test]
    fn text_receipt_progress_defers_retry_without_releasing_unconsumed_records() {
        let mut queue = PendingOutput::default();
        for _ in 0..16 {
            queue.push("I421100".into()).unwrap();
        }
        let mut window = TextWindow::default();
        let now = Instant::now();
        for sequence in 1..=16 {
            assert_eq!(window.next(&queue, 1, now).unwrap().unwrap().0, sequence);
            window.sent(sequence, now);
        }
        window.receipt(&mut queue, 8, 0, 1).unwrap();
        let progressed = now + Duration::from_millis(200);
        assert!(window.next(&queue, 1, progressed).unwrap().is_none());
        assert!(window.next(&queue, 1, now + TEXT_RETRY).unwrap().is_none());
        assert_eq!(queue.records.len(), 16);
        assert_eq!(window.next(&queue, 1, progressed + TEXT_RETRY).unwrap().unwrap().0, 9);
    }

    #[test]
    fn text_received_credit_stops_at_sixteen_until_gameplay_consumes_records() {
        let mut queue = PendingOutput::default();
        for _ in 0..20 { queue.push("I421100".into()).unwrap(); }
        let mut window = TextWindow::default();
        let now = Instant::now();
        for sequence in 1..=16 {
            let next = window.next(&queue, 1, now).unwrap().unwrap();
            assert_eq!(next.0, sequence);
            window.sent(sequence, now);
        }
        window.receipt(&mut queue, 16, 0, 1).unwrap();
        assert!(window.next(&queue, 1, now).unwrap().is_none());
        let retry = now + TEXT_RETRY;
        window.receipt(&mut queue, 16, 0, 2).unwrap();
        assert!(window.next(&queue, 1, retry).unwrap().is_none());
        assert_eq!(queue.records.len(), 20);
        window.receipt(&mut queue, 16, 1, 3).unwrap();
        assert_eq!(queue.records.len(), 19);
        assert_eq!(window.next(&queue, 1, retry).unwrap().unwrap().0, 17);
    }

    #[test]
    fn text_retries_only_the_first_missing_record_after_partial_receipt_progress() {
        for received in [1, 4] {
            let mut queue = PendingOutput::default();
            for _ in 0..16 {
                queue.push("I421100".into()).unwrap();
            }
            let mut window = TextWindow::default();
            let now = Instant::now();
            window.receipt(&mut queue, 0, 0, 1).unwrap();
            for sequence in 1..=16 {
                assert_eq!(window.next(&queue, 1, now).unwrap().unwrap().0, sequence);
                window.sent(sequence, now);
            }
            window.receipt(&mut queue, received, 0, 2).unwrap();
            let progressed = now + Duration::from_millis(100);
            assert!(window.next(&queue, 1, progressed).unwrap().is_none());
            let retry = progressed + TEXT_RETRY;
            let missing = window.next(&queue, 1, retry).unwrap().unwrap();
            assert_eq!(missing.0, received + 1);
            window.sent(missing.0, retry);
            assert!(window.next(&queue, 1, retry).unwrap().is_none());
            assert_eq!(queue.records.len(), 16);
            window.receipt(&mut queue, missing.0, 0, 3).unwrap();
            let next_gap = window.next(&queue, 1, retry).unwrap().unwrap();
            assert_eq!(next_gap.0, missing.0 + 1);
            window.sent(next_gap.0, retry);
            assert!(window.next(&queue, 1, retry).unwrap().is_none());
            window.receipt(&mut queue, 16, 16, 4).unwrap();
            assert!(queue.is_empty());
            assert!(window.next(&queue, 1, retry).unwrap().is_none());
        }
    }

    #[test]
    fn text_receipts_require_complete_matching_native_publication() {
        assert_eq!(
            text_envelope(1, 1, "I421100").unwrap(),
            "@J10000000001000000000102742|I421100;"
        );
        let prefix =
            "call Preload( \"SMASHCRAFT TEXT ACK v=1 build=focus epoch=1 slot=0 received=7 consumed=5 revision=2 chat=0 chatState=0 chatFrame=1\" )\n";
        assert_eq!(text_receipt(prefix, "focus", 1, 0).unwrap(), None);
        let complete = format!("{prefix}endfunction\n");
        assert_eq!(text_receipt(&complete, "focus", 1, 0).unwrap(), Some(TextReceipt { received: 7, consumed: 5, revision: 2, chat: 0, chat_state: 0 }));
        assert!(text_receipt(&complete, "focus", 2, 0).is_err());
        assert!(text_receipt(&complete, "focus", 1, 1).is_err());
        assert!(text_envelope(1, 1, "I4;incomplete").is_err());
    }

    #[test]
    fn chat_receipts_preserve_queue_until_consumed_and_close_requires_neutral() {
        let receipt = |state| format!("call Preload( \"SMASHCRAFT TEXT ACK v=1 build=chat epoch=1 slot=0 received=1 consumed=1 revision=9 chat=2 chatState={state} chatFrame=1\" )\nendfunction\n");
        for state in 0..=3 {
            let parsed = text_receipt(&receipt(state), "chat", 1, 0).unwrap().unwrap();
            assert_eq!((parsed.chat, parsed.chat_state), (2, state));
        }
        assert!(text_receipt(&receipt(4), "chat", 1, 0).is_err());
        let mut queue = PendingOutput::default();
        queue.push("ACK1|2|PAUSE|91".into()).unwrap();
        let mut window = TextWindow::default();
        let now = Instant::now();
        window.receipt(&mut queue, 0, 0, 1).unwrap();
        let (sequence, _) = window.next(&queue, 1, now).unwrap().unwrap();
        window.sent(sequence, now);
        window.receipt(&mut queue, 1, 0, 2).unwrap();
        assert!(!queue.is_empty());
        window.receipt(&mut queue, 1, 1, 3).unwrap();
        assert!(queue.is_empty());
        assert!(window.next(&queue, 1, now + TEXT_RETRY).unwrap().is_none());

        let mut input = FocusInput { eligible: true, armed: true, gameplay_armed: true, physical: State::default(), accept_since_ns: 0 };
        let down = evdev::InputEvent::new(evdev::EventType::KEY.0, Key::BTN_SOUTH.0, 1);
        let up = evdev::InputEvent::new(evdev::EventType::KEY.0, Key::BTN_SOUTH.0, 0);
        input.observe(false, false, 0);
        assert!(!input.accepts(&[None; 6], down, false, false).unwrap());
        input.observe(true, false, 0);
        input.rearm(0, false, false);
        assert!(!input.armed);
        // Another player's Start resumes while this player still holds Attack.
        input.rearm(0, false, true);
        assert!(!input.gameplay_armed);
        assert!(!input.accepts(&[None; 6], up, false, true).unwrap());
        assert!(input.accepts(&[None; 6], down, false, true).unwrap());
    }

    #[test]
    fn focus_return_requires_neutral_and_preserves_start_during_pause() {
        let ranges = [None; 6];
        let down = evdev::InputEvent::new(evdev::EventType::KEY.0, Key::BTN_SOUTH.0, 1);
        let up = evdev::InputEvent::new(evdev::EventType::KEY.0, Key::BTN_SOUTH.0, 0);
        let mut input = FocusInput {
            eligible: true,
            armed: true,
            gameplay_armed: true,
            physical: State::default(),
            accept_since_ns: 0,
        };
        assert!(input.accepts(&ranges, down, false, true).unwrap());
        assert!(input.observe(false, true, 0));
        assert!(!input.accepts(&ranges, up, false, true).unwrap());
        assert!(!input.accepts(&ranges, down, false, true).unwrap());
        input.observe(true, false, 0);
        input.rearm(0, false, true);
        assert!(!input.armed);
        assert!(!input.accepts(&ranges, up, false, true).unwrap());
        assert!(input.armed);
        assert!(input.accepts(&ranges, down, false, true).unwrap());
        // Paused gameplay disarms independently of the focus-qualified Start control.
        assert!(!input.accepts(&ranges, down, false, false).unwrap());
        assert!(input.armed);
        assert!(!input.gameplay_armed);
        input.gameplay_armed = true;
        input.rearm(0, false, false);
        assert!(!input.gameplay_armed);
        let start = evdev::InputEvent::new(evdev::EventType::KEY.0, Key::BTN_START.0, 1);
        assert!(pause_request_wire(&start, &mut false, 7, 3, true, !input.armed).is_some());
        assert!(!input.accepts(&ranges, down, false, true).unwrap());
        assert!(!input.accepts(&ranges, up, false, true).unwrap());
        assert!(input.accepts(&ranges, down, false, true).unwrap());
        input.observe(false, true, 1);
        input.observe(true, false, 2);
        // A recovered queue item older than the focus boundary cannot rearm.
        assert!(!input.accepts(&ranges, up, false, true).unwrap());
        assert!(!input.armed);
        input.rearm(2, false, true);
        assert!(input.armed);
        assert!(!input.accepts(&ranges, down, false, true).unwrap());
    }

    fn release_for_focus_loss(
        state: &mut State,
        edges: &mut BTreeMap<u32, Edges>,
        snapshots: &mut BTreeMap<u32, State>,
        next_frame: u32,
        segment: FrameSegment,
        now: u128,
    ) -> Result<u32, String> {
        // Already assigned rows remain immutable, including the open frame.
        let after_assigned = edges
            .keys()
            .chain(snapshots.keys())
            .max()
            .copied()
            .map_or(next_frame, |frame| frame.saturating_add(1));
        let frame = frame_at(now.max(segment.epoch_ns), segment)?
            .max(next_frame)
            .max(after_assigned);
        if frame > LAST_FRAME {
            return Err("focus release exceeds supported frame range".into());
        }
        edges.entry(frame).or_default().released |= action_state(*state);
        *state = State::default();
        snapshots.insert(frame, *state);
        Ok(frame)
    }

    fn apply_event(
        ranges: &[Option<evdev::AbsInfo>; 6],
        state: &mut State,
        event: evdev::InputEvent,
        edges: &mut BTreeMap<u32, Edges>,
        snapshots: &mut BTreeMap<u32, State>,
        earliest_unwritten: u32,
        segment: FrameSegment,
        trace: bool,
    ) -> Result<(), String> {
        let summary = event.destructure();
        if matches!(
            &summary,
            EventSummary::Synchronization(_, evdev::SynchronizationCode::SYN_DROPPED, _)
        ) {
            return Err(
                "kernel reported SYN_DROPPED; lost original input edges cannot be reconstructed"
                    .into(),
            );
        }
        let mapped = match &summary {
            EventSummary::Key(_, key, _) => matches!(
                *key,
                Key::BTN_SOUTH
                    | Key::BTN_EAST
                    | Key::BTN_WEST
                    | Key::BTN_NORTH
                    | Key::BTN_TL
                    | Key::BTN_TR
            ),
            EventSummary::AbsoluteAxis(_, code, _) => matches!(
                *code,
                Abs::ABS_X | Abs::ABS_Y | Abs::ABS_RX | Abs::ABS_RY | Abs::ABS_Z | Abs::ABS_RZ
            ),
            _ => false,
        };
        if !mapped {
            return Ok(());
        }
        let timestamp = event_ns(&event)?;
        let frame = frame_at(timestamp, segment)?;
        if frame < earliest_unwritten {
            return Err(format!(
                "late kernel event belongs to frame {frame}, already-published cursor is {earliest_unwritten}; original frame retained and journal stopped"
            ));
        }
        let mut edge = Edges::default();
        let before = action_state(*state);
        let before_x = i16::from(before & MOVE_RIGHT != 0) - i16::from(before & MOVE_LEFT != 0);
        let before_y = i16::from(before & MOVE_UP != 0) - i16::from(before & MOVE_DOWN != 0);
        update_state(ranges, state, event);
        let after = action_state(*state);
        edge.pressed = after & !before;
        edge.released = before & !after;
        let e = edges.entry(frame).or_default();
        let special_pending = e.pressed & SPECIAL != 0;
        e.pressed |= edge.pressed;
        e.released |= edge.released;
        if edge.pressed & SPECIAL != 0 && !special_pending {
            let (x, y) = melee_stick(state.x, state.y);
            e.special_x = x.signum();
            e.special_y = -y.signum();
        }
        if edge.pressed & (LEFT_TRIGGER | RIGHT_TRIGGER) != 0 {
            let (x, y) = melee_stick(state.x, state.y);
            e.dodge_x = x.signum();
            e.dodge_y = -y.signum();
        }
        if edge.pressed & MOVE_UP != 0 {
            e.ledge = 1;
        }
        if edge.pressed & MOVE_DOWN != 0 {
            e.ledge = -1;
        }
        let after_x = i16::from(after & MOVE_RIGHT != 0) - i16::from(after & MOVE_LEFT != 0);
        let after_y = i16::from(after & MOVE_UP != 0) - i16::from(after & MOVE_DOWN != 0);
        if (after_x != 0 && after_x != before_x) || (after_y != 0 && after_y != before_y) {
            e.sdi_x = after_x;
            e.sdi_y = after_y;
        }
        for (bit, x, y) in [
            (MOVE_LEFT, -1, 0),
            (MOVE_RIGHT, 1, 0),
            (MOVE_DOWN, 0, -1),
            (MOVE_UP, 0, 1),
            (SMASH_LEFT, -1, 0),
            (SMASH_RIGHT, 1, 0),
            (SMASH_DOWN, 0, -1),
            (SMASH_UP, 0, 1),
        ] {
            if edge.pressed & bit != 0 {
                e.throw_x = (e.throw_x + x).clamp(-127, 127);
                e.throw_y = (e.throw_y + y).clamp(-127, 127);
            }
        }
        snapshots.insert(frame, *state);
        if trace {
            println!(
                "event mono_ns={timestamp} frame={frame} held={after} pressed={} released={}",
                edge.pressed, edge.released
            );
        }
        Ok(())
    }

    fn publish(
        dir: &Path,
        build: &str,
        epoch: u32,
        slot: u32,
        first: u32,
        rows: &[String],
    ) -> io::Result<()> {
        let base = dir.join(format!(
            "smashcraft-journal-{build}-e{epoch}-s{slot}-n{first}"
        ));
        publish_vocabulary(&base, &encode_packet(epoch, first, rows))
    }

    fn submit_packet(
        dir: &Path,
        build: &str,
        epoch: u32,
        slot: u32,
        first: u32,
        rows: &[String],
        mailbox: &mut Option<MailboxSender>,
    ) -> io::Result<()> {
        if let Some(mailbox) = mailbox.as_mut() {
            mailbox.enqueue(encode_packet(epoch, first, rows))
        } else {
            publish(dir, build, epoch, slot, first, rows)
        }
    }

    fn vocabulary_path(base: &Path, suffix: &str) -> PathBuf {
        let mut path = base.as_os_str().to_os_string();
        path.push(suffix);
        path.into()
    }

    fn publish_symbol(target: &Path, symbol: u8) -> io::Result<()> {
        let temp = vocabulary_path(target, &format!(".{}.tmp", std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        let result = (|| {
            writeln!(file, "function PreloadFiles takes nothing returns nothing")?;
            writeln!(
                file,
                "call BlzSetAbilityTooltip('$wsl', \"{}\", 0)",
                char::from(symbol)
            )?;
            writeln!(file, "endfunction")?;
            drop(file);
            // A hard link publishes complete bytes without replacing a peer or
            // previously published immutable symbol, including after a restart.
            fs::hard_link(&temp, target)
        })();
        let cleanup = fs::remove_file(temp);
        result.and(cleanup)
    }

    fn publish_vocabulary(base: &Path, wire: &str) -> io::Result<()> {
        // The marker is an alphabet index. '|' is the sole additional fixed
        // script needed by the existing ACK1 control wire.
        if wire.is_empty()
            || wire.len() >= ALPHABET.len()
            || !wire
                .bytes()
                .all(|symbol| ALPHABET.contains(&symbol) || symbol == b'|')
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid vocabulary payload",
            ));
        }
        let marker = vocabulary_path(base, "-length.pld");
        if marker.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "immutable journal packet already exists",
            ));
        }
        for (offset, symbol) in wire.bytes().enumerate() {
            publish_symbol(&vocabulary_path(base, &format!("-c{offset}.pld")), symbol)?;
        }
        // Readers do not inspect symbols until this final publication succeeds.
        publish_symbol(&marker, ALPHABET[wire.len()])
    }

    fn control_path(dir: &Path, build: &str, epoch: u32, slot: u32, sequence: u32) -> PathBuf {
        dir.join(format!(
            "smashcraft-journal-control-{build}-e{epoch}-s{slot}-n{sequence}.txt"
        ))
    }

    fn publish_control_ack(
        dir: &Path,
        build: &str,
        epoch: u32,
        slot: u32,
        sequence: u32,
        state: ControlState,
        frame: u32,
    ) -> io::Result<()> {
        let base = dir.join(format!(
            "smashcraft-journal-ack-{build}-e{epoch}-s{slot}-n{sequence}"
        ));
        let state = match state {
            ControlState::Started => "START",
            ControlState::Ended => "END",
            ControlState::PausePrepare => "PREPARE",
            ControlState::PauseCommit => "COMMIT",
            ControlState::Paused => "PAUSE",
            ControlState::Resumed => "RESUME",
        };
        publish_vocabulary(&base, &format!("ACK1|{sequence}|{state}|{frame}"))
    }

    fn submit_control_ack(
        dir: &Path,
        build: &str,
        epoch: u32,
        slot: u32,
        sequence: u32,
        state: ControlState,
        frame: u32,
        mailbox: &mut Option<MailboxSender>,
    ) -> io::Result<()> {
        if let Some(mailbox) = mailbox.as_mut() {
            let state = match state {
                ControlState::Started => "START",
            ControlState::Ended => "END",
            ControlState::PausePrepare => "PREPARE",
                ControlState::PauseCommit => "COMMIT",
                ControlState::Paused => "PAUSE",
                ControlState::Resumed => "RESUME",
            };
            mailbox.enqueue(format!("ACK1|{sequence}|{state}|{frame}"))
        } else {
            publish_control_ack(dir, build, epoch, slot, sequence, state, frame)
        }
    }

    #[test]
    fn vocabulary_publication_preserves_packets_acks_and_immutable_commit_boundary() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("journal-vocabulary-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let read_symbol = |path: PathBuf| {
            let script = fs::read_to_string(path).unwrap();
            let symbol = script.strip_prefix("function PreloadFiles takes nothing returns nothing\ncall BlzSetAbilityTooltip('$wsl', \"")
                .unwrap().strip_suffix("\", 0)\nendfunction\n").unwrap();
            assert_eq!(symbol.len(), 1);
            symbol.as_bytes()[0]
        };
        let read_wire = |base: &Path| {
            let marker = read_symbol(vocabulary_path(base, "-length.pld"));
            let length = ALPHABET
                .iter()
                .position(|symbol| *symbol == marker)
                .unwrap();
            (0..length)
                .map(|offset| {
                    char::from(read_symbol(vocabulary_path(
                        base,
                        &format!("-c{offset}.pld"),
                    )))
                })
                .collect::<String>()
        };
        let press = encode_row(
            State {
                sources: 1,
                ..State::default()
            },
            0,
            Edges::default(),
        );
        let release = encode_row(State::default(), ATTACK, Edges::default());
        let rows = [press, release];
        // The filename and I4 header both retain frame 4 (including delay).
        publish(&dir, "vocabulary", 91, 2, 4, &rows).unwrap();
        let base = dir.join("smashcraft-journal-vocabulary-e91-s2-n4");
        let wire = encode_packet(91, 4, &rows);
        assert_eq!(read_wire(&base), wire);
        assert_eq!(
            publish(&dir, "vocabulary", 91, 2, 4, &["0".into()])
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(read_wire(&base), wire);
        for (sequence, state, expected) in [
            (1, ControlState::PausePrepare, "ACK1|1|PREPARE|6"),
            (2, ControlState::Paused, "ACK1|2|PAUSE|6"),
            (3, ControlState::Resumed, "ACK1|3|RESUME|6"),
        ] {
            publish_control_ack(&dir, "vocabulary", 91, 2, sequence, state, 6).unwrap();
            assert_eq!(
                read_wire(&dir.join(format!(
                    "smashcraft-journal-ack-vocabulary-e91-s2-n{sequence}"
                ))),
                expected
            );
        }
        // A collision halfway through must never expose a committed packet.
        let partial = dir.join("partial");
        let collision = vocabulary_path(&partial, "-c1.pld");
        publish_symbol(&collision, b'Z').unwrap();
        assert_eq!(
            publish_vocabulary(&partial, "I412300").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(!vocabulary_path(&partial, "-length.pld").exists());
        assert_eq!(read_symbol(collision), b'Z');
        let invalid = dir.join("invalid");
        assert_eq!(
            publish_vocabulary(&invalid, "unsafe\"").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(!vocabulary_path(&invalid, "-c0.pld").exists());
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            assert_eq!(path.extension().unwrap(), "pld");
            let symbol = read_symbol(path.clone());
            assert!(ALPHABET.contains(&symbol) || symbol == b'|');
            fs::remove_file(path).unwrap();
        }
        fs::remove_dir(dir).unwrap();
    }

    /// How a pad's driver names its face buttons. The capture keys on
    /// positions (BTN_SOUTH/EAST/NORTH/WEST). Linux drivers disagree: Sony's
    /// hid-playstation reports positions, while xpad and other Xbox-style
    /// drivers report labels, BTN_X (0x133, the code of BTN_NORTH) for the
    /// left button and BTN_Y (0x134, BTN_WEST) for the top one. SDL decides
    /// by vendor in its Linux joystick mapping; wc3-controller reads SDL's
    /// positions, so both helpers resolve every pad the same way.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum FaceLabels {
        Positions,
        Xbox,
    }

    const SONY_VENDOR: u16 = 0x054c;

    impl FaceLabels {
        fn of(id: evdev::InputId) -> Self {
            if id.vendor() == SONY_VENDOR { Self::Positions } else { Self::Xbox }
        }

        fn position(self, code: u16) -> u16 {
            match (self, code) {
                (Self::Xbox, 0x133) => Key::BTN_WEST.0,
                (Self::Xbox, 0x134) => Key::BTN_NORTH.0,
                _ => code,
            }
        }

        fn positional(self, event: evdev::InputEvent) -> evdev::InputEvent {
            if event.event_type() != evdev::EventType::KEY {
                return event;
            }
            let mut raw: libc::input_event = event.into();
            raw.code = self.position(raw.code);
            raw.into()
        }
    }

    #[test]
    fn xbox_face_labels_map_x_to_special_and_y_to_jump() {
        // Codes exactly as the kernel's xpad driver reports a wired Xbox One S
        // pad (045e:02ea): A BTN_A, B BTN_B, X BTN_X, Y BTN_Y.
        let xbox = FaceLabels::of(evdev::InputId::new(evdev::BusType::BUS_USB, 0x045e, 0x02ea, 0x0301));
        assert_eq!(xbox, FaceLabels::Xbox);
        let ranges = [None; 6];
        let held = |code: u16| {
            let mut state = State::default();
            let event = libc::input_event {
                time: libc::timeval { tv_sec: 1, tv_usec: 5 },
                type_: evdev::EventType::KEY.0,
                code,
                value: 1,
            };
            let positional = xbox.positional(event.into());
            assert_eq!(event_ns(&positional).unwrap(), 1_000_005_000);
            update_state(&ranges, &mut state, positional);
            action_state(state)
        };
        // BTN_A, BTN_B, BTN_X and BTN_Y in linux/input-event-codes.h.
        assert_eq!(held(0x130), ATTACK);
        assert_eq!(held(0x131), JUMP);
        assert_eq!(held(0x133), SPECIAL);
        assert_eq!(held(0x134), JUMP);
        // hid-playstation reports positions: square BTN_WEST, triangle BTN_NORTH.
        let sony = FaceLabels::of(evdev::InputId::new(evdev::BusType::BUS_USB, SONY_VENDOR, 0x0ce6, 0x8111));
        assert_eq!(sony.position(Key::BTN_WEST.0), Key::BTN_WEST.0);
        assert_eq!(sony.position(Key::BTN_NORTH.0), Key::BTN_NORTH.0);
    }

    #[derive(Debug, PartialEq, Eq)]
    struct DeviceIdentity {
        id: evdev::InputId,
        name: Option<String>,
        phys: Option<String>,
        uniq: Option<String>,
    }

    impl DeviceIdentity {
        fn of(device: &RawDevice) -> Self {
            Self {
                id: device.input_id(),
                name: device.name().map(str::to_owned),
                phys: device.physical_path().filter(|s| !s.is_empty()).map(str::to_owned),
                uniq: device.unique_name().filter(|s| !s.is_empty()).map(str::to_owned),
            }
        }

        fn reconnectable(&self) -> bool {
            self.phys.is_some() || self.uniq.is_some()
        }

        fn from_sysfs(device: &Path) -> io::Result<Self> {
            let field = |name: &str| -> io::Result<String> {
                let value = fs::read_to_string(device.join(name))?;
                Ok(value.strip_suffix('\n').unwrap_or(&value).to_owned())
            };
            let hex = |name: &str| -> io::Result<u16> {
                u16::from_str_radix(&field(name)?, 16)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
            };
            let optional = |value: String| if value.is_empty() { None } else { Some(value) };
            Ok(Self {
                id: evdev::InputId::new(evdev::BusType(hex("id/bustype")?), hex("id/vendor")?, hex("id/product")?, hex("id/version")?),
                name: Some(field("name")?),
                phys: optional(field("phys")?),
                uniq: optional(field("uniq")?),
            })
        }
    }

    fn matching_devices(identity: &DeviceIdentity) -> io::Result<Vec<PathBuf>> {
        let mut matches = Vec::new();
        // Closing an evdev fd can wait for kernel readers. Enumerating identity
        // through sysfs avoids opening and closing every unrelated input device
        // on the same thread that captures frames and services delivery.
        for entry in fs::read_dir("/sys/class/input")? {
            let entry = entry?;
            if !entry.file_name().to_str().is_some_and(|s| s.starts_with("event")) {
                continue;
            }
            let candidate = match DeviceIdentity::from_sysfs(&entry.path().join("device")) {
                Ok(candidate) => candidate,
                Err(error) if error.kind() == io::ErrorKind::NotFound
                    || error.raw_os_error() == Some(libc::ENODEV) => continue,
                Err(error) => return Err(error),
            };
            if candidate == *identity {
                matches.push(Path::new("/dev/input").join(entry.file_name()));
            }
        }
        Ok(matches)
    }

    #[test]
    #[ignore = "read-only native scan timing; requires access to /dev/input"]
    fn reconnect_scan_latency_probe() {
        let absent = DeviceIdentity {
            id: evdev::InputId::new(evdev::BusType::BUS_USB, 0x045e, 0x02ea, 1),
            name: Some("Smashcraft disconnected measurement".into()),
            phys: Some("smashcraft-disconnected-measurement".into()),
            uniq: None,
        };
        for sample in 0..3 {
            let scan = Instant::now();
            assert!(matching_devices(&absent).unwrap().is_empty());
            eprintln!("scan_sample={sample} total_us={}", scan.elapsed().as_micros());
        }
    }

    #[test]
    fn reconnect_sysfs_identity_retains_exact_discriminator() {
        let dir = env::temp_dir().join(format!("reconnect-identity-{}", std::process::id()));
        fs::create_dir_all(dir.join("id")).unwrap();
        for (field, value) in [("id/bustype", "0003"), ("id/vendor", "045e"), ("id/product", "02ea"), ("id/version", "0301"), ("name", "Xbox"), ("phys", "usb-port-1/input0"), ("uniq", "")] {
            fs::write(dir.join(field), format!("{value}\n")).unwrap();
        }
        let identity = DeviceIdentity::from_sysfs(&dir).unwrap();
        assert_eq!(identity, DeviceIdentity {
            id: evdev::InputId::new(evdev::BusType::BUS_USB, 0x045e, 0x02ea, 0x0301),
            name: Some("Xbox".into()), phys: Some("usb-port-1/input0".into()), uniq: None,
        });
        fs::write(dir.join("phys"), "usb-port-2/input0\n").unwrap();
        assert_ne!(DeviceIdentity::from_sysfs(&dir).unwrap(), identity);
        for field in ["id/bustype", "id/vendor", "id/product", "id/version", "name", "phys", "uniq"] {
            fs::remove_file(dir.join(field)).unwrap();
        }
        fs::remove_dir(dir.join("id")).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    // Live menu/Start gating cannot depend on the gameplay queue, which can
    // retain original events while a pause or start publication is pending.
    struct RecoveryGate {
        ready: bool,
        physical: State,
        start: bool,
        since_ns: u128,
    }

    impl RecoveryGate {
        fn restored(&mut self, physical: State, start: bool, ns: u128) {
            self.physical = physical;
            self.start = start;
            self.since_ns = ns;
            self.ready = action_state(physical) == 0 && !start;
        }

        fn observe(&mut self, axes: &[Option<evdev::AbsInfo>; 6], event: evdev::InputEvent) {
            update_state(axes, &mut self.physical, event);
            if let EventSummary::Key(_, Key::BTN_START, value) = event.destructure() {
                self.start = value != 0;
            }
            if action_state(self.physical) == 0 && !self.start {
                self.ready = true;
            }
        }
    }

    #[test]
    fn reconnect_identity_requires_a_stable_discriminator_and_excludes_another_pad() {
        let identity = |phys: Option<&str>| DeviceIdentity {
            id: evdev::InputId::new(evdev::BusType::BUS_USB, 0x045e, 0x02ea, 1),
            name: Some("Xbox One S Controller".into()),
            phys: phys.map(str::to_owned),
            uniq: None,
        };
        assert!(!identity(None).reconnectable());
        assert!(identity(Some("usb-port-1/input0")).reconnectable());
        assert_eq!(identity(Some("usb-port-1/input0")), identity(Some("usb-port-1/input0")));
        assert_ne!(identity(Some("usb-port-1/input0")), identity(Some("usb-port-2/input0")));
    }

    #[test]
    fn reconnect_retains_original_tap_and_releases_held_input_at_detection() {
        let ranges = [None; 6];
        let segment = FrameSegment { epoch_ns: 0, first_frame: 1 };
        let mut queue = PendingInput::default();
        queue.push(timed_button(20_000_000, true), false, false).unwrap();
        queue.push(timed_button(21_000_000, false), false, false).unwrap();
        queue.push(timed_button(40_000_000, true), false, false).unwrap();
        queue.push_capture(Capture::Disconnected(60_000_000)).unwrap();
        // A paused/control-pending drain must retain both the edges and boundary.
        assert!(queue.pop(true, 0).unwrap().is_none());
        let mut state = State::default();
        let mut edges = BTreeMap::new();
        let mut snapshots = BTreeMap::new();
        let mut release = None;
        while let Some(capture) = queue.pop(false, 0).unwrap() {
            match capture {
                Capture::Event(event, ..) => apply_event(&ranges, &mut state, event, &mut edges, &mut snapshots, 1, segment, false).unwrap(),
                Capture::Disconnected(ns) => release = Some(release_for_focus_loss(&mut state, &mut edges, &mut snapshots, 1, segment, ns).unwrap()),
                Capture::Reconnected { .. } => panic!("unexpected reconnect"),
            }
        }
        assert_eq!(edges[&2].pressed, ATTACK);
        assert_eq!(edges[&2].released, ATTACK);
        assert_eq!(edges[&3].pressed, ATTACK);
        assert_eq!(release, Some(4));
        assert_eq!(edges[&4].released, ATTACK);
        assert_eq!(snapshots[&4], State::default());
        assert_eq!(state, State::default());
        assert_eq!(frame_at(100_000_000, segment).unwrap(), 7);
    }

    #[test]
    fn reconnect_held_state_requires_neutral_before_gameplay_menu_and_pause() {
        let ranges = [None; 6];
        let held = State { sources: 1, ..State::default() };
        let mut input = FocusInput { eligible: true, armed: true, gameplay_armed: true, physical: held, accept_since_ns: 0 };
        let mut recovery = RecoveryGate { ready: false, physical: held, start: true, since_ns: 0 };
        recovery.restored(held, true, 10_000_000);
        input.restore(held, true, 10_000_000, true);
        let mut menu = MenuInput::default();
        menu.observe(Some(MenuPhase::Character), held, true, 10_000_000);
        let up = timed_button(11_000_000, false);
        assert!(!input.accepts(&ranges, up, true, true).unwrap());
        recovery.observe(&ranges, up);
        assert!(!recovery.ready);
        assert_eq!(menu.press(held, State::default(), true, true, 11_000_000), None);
        let start_up = evdev::InputEvent::new(evdev::EventType::KEY.0, Key::BTN_START.0, 0);
        recovery.observe(&ranges, start_up);
        assert!(recovery.ready);
        input.rearm(12_000_000, false, true);
        assert!(input.accepts(&ranges, timed_button(13_000_000, true), false, true).unwrap());
        menu.observe(Some(MenuPhase::Character), State::default(), false, 12_000_000);
        assert_eq!(menu.press(State::default(), held, false, false, 13_000_000), Some("n"));
        let start_down = evdev::InputEvent::new(evdev::EventType::KEY.0, Key::BTN_START.0, 1);
        assert!(pause_request_wire(&start_down, &mut false, 7, 3, true, !recovery.ready).is_some());
        // Recovery of neutral state also permits the first genuinely new tap.
        input.restore(State::default(), false, 20_000_000, true);
        assert!(input.accepts(&ranges, timed_button(21_000_000, true), false, true).unwrap());
    }

    fn device_snapshot(device: &RawDevice) -> io::Result<([Option<evdev::AbsInfo>; 6], State, bool)> {
        let mut axes = [None; 6];
        for (code, info) in device.get_absinfo()? {
            if code.0 < axes.len() as u16 {
                axes[code.0 as usize] = Some(info);
            }
        }
        for required in [
            Abs::ABS_X,
            Abs::ABS_Y,
            Abs::ABS_RX,
            Abs::ABS_RY,
            Abs::ABS_Z,
            Abs::ABS_RZ,
        ] {
            if axes.get(required.0 as usize).is_none_or(Option::is_none) {
                return Err(io::Error::new(io::ErrorKind::InvalidData, format!("selected device lacks required Linux Xbox axis {required:?}")));
            }
        }
        let key_state = device.get_key_state()?;
        let labels = FaceLabels::of(device.input_id());
        let axis_value = |code: Abs| axes[code.0 as usize].map_or(0, |info| info.value());
        let mut state = State {
            x: normalized_axis(&axes, Abs::ABS_X, axis_value(Abs::ABS_X)),
            y: normalized_axis(&axes, Abs::ABS_Y, axis_value(Abs::ABS_Y)),
            cx: normalized_axis(&axes, Abs::ABS_RX, axis_value(Abs::ABS_RX)),
            cy: normalized_axis(&axes, Abs::ABS_RY, axis_value(Abs::ABS_RY)),
            lt: normalized_trigger(&axes, Abs::ABS_Z, axis_value(Abs::ABS_Z)),
            rt: normalized_trigger(&axes, Abs::ABS_RZ, axis_value(Abs::ABS_RZ)),
            ..State::default()
        };
        for key in [Key::BTN_SOUTH, Key::BTN_EAST, Key::BTN_WEST, Key::BTN_NORTH, Key::BTN_TL, Key::BTN_TR] {
            if key_state.contains(key) {
                update_state(&axes, &mut state, evdev::InputEvent::new(evdev::EventType::KEY.0, labels.position(key.0), 1));
            }
        }
        Ok((axes, state, key_state.contains(Key::BTN_START)))
    }

    fn run() -> Result<(), String> {
        let mut o = options()?;
        let mut ready_after_ns = clock_ns(libc::CLOCK_REALTIME).map_err(|e| e.to_string())?;
        if (!o.follow_matches && o.epoch == 0) || o.slot > 3 || o.delay > 64 {
            return Err("epoch must be positive, slot 0..3, delay 0..64".into());
        }
        if o.first_frame == 0 || o.first_frame > LAST_FRAME - o.delay {
            return Err("first-frame must be a valid positive capture-segment frame".into());
        }
        fs::create_dir_all(&o.out).map_err(|e| e.to_string())?;
        let ingresses = [o.mailbox_display.is_some(), o.editbox_display.is_some(), o.text_out.is_some()];
        if ingresses.iter().filter(|selected| **selected).count() > 1 {
            return Err("select only one keyboard ingress".into());
        }
        // Text output stands in for the edit box of a headless client.
        let editbox = o.editbox_display.is_some() || o.text_out.is_some();
        if !o.follow_matches { clear_quiescent(&o.out, &o.build, o.epoch, o.slot)?; }
        let typist = match (&o.text_out, o.editbox_display.as_ref().or(o.mailbox_display.as_ref())) {
            (Some(path), _) => Some(Typist::file(path)?),
            (None, Some(display)) => Some(Typist::window(
                display,
                crate::focus::Target {
                    display: display.clone(),
                    window: o.window.ok_or("keyboard output requires --x11-window")?,
                    pid: o.pid.ok_or("keyboard output requires --pid")?,
                    niri_window: o.niri_window,
                    wlr_app_id: o.wlr_app_id.clone(),
                },
            )?),
            (None, None) => None,
        };
        let mut mailbox = typist
            .map(|typist| MailboxSender::new(&o.out, &o.build, o.epoch, o.slot, o.trace, editbox, typist))
            .transpose()?;
        let device =
            RawDevice::open(&o.device).map_err(|e| format!("open {}: {e}", o.device.display()))?;
        set_monotonic_event_clock(&device)
            .map_err(|e| format!("set EVIOCSCLOCKID(CLOCK_MONOTONIC): {e}"))?;
        set_nonblocking(&device).map_err(|e| format!("set evdev nonblocking mode: {e}"))?;
        eprintln!(
            "source={} name={:?} clock=CLOCK_MONOTONIC epoch_ns={} frame_rule=segment_frame+floor((t-segment_epoch)*60/1e9) first={} delay={} pause_policy=sequenced-map-control",
            o.device.display(),
            device.name(),
            o.epoch_ns,
            o.first_frame,
            o.delay
        );
        if let Some(path) = &o.ready_file {
            eprintln!("readiness_receipt={}", path.display());
        }
        let (mut axes, physical, mut start_held) = device_snapshot(&device).map_err(|e| e.to_string())?;
        let mut capture_axes = axes;
        let mut state = State::default();
        let identity = DeviceIdentity::of(&device);
        let labels = FaceLabels::of(device.input_id());
        let can_reconnect = identity.reconnectable()
            && matching_devices(&identity).map_err(|e| format!("enumerate controller identity: {e}"))?.len() == 1;
        eprintln!("controller_identity={identity:?} face_labels={labels:?} automatic_reconnect={can_reconnect}");
        let mut device = Some(device);
        let mut next_reconnect = Instant::now();
        let mut ambiguous = false;
        let mut recovery = RecoveryGate { ready: action_state(physical) == 0 && !start_held, physical, start: start_held, since_ns: 0 };
        let mut row_state = state;
        let mut edges = BTreeMap::<u32, Edges>::new();
        let mut snapshots = BTreeMap::<u32, State>::new();
        let mut previous = action_state(state);
        let mut next_frame = o.first_frame + o.delay;
        let mut segment = FrameSegment {
            epoch_ns: o.epoch_ns,
            first_frame: next_frame,
        };
        let mut control_sequence = 1;
        let mut focus_input = FocusInput {
            eligible: true,
            armed: action_state(physical) == 0,
            gameplay_armed: action_state(physical) == 0,
            physical,
            accept_since_ns: o.epoch_ns,
        };
        let control_clock = ControlClock::new(&o.out).map_err(|e| e.to_string())?;
        let mut pending_input = PendingInput::default();
        let mut ready_publication_ns = 0;
        let mut waiting_ready = o.follow_matches;
        let mut waiting_start = false;
        let mut menu_input = MenuInput::default();
        let mut menu = None;
        let mut ended = false;
        let mut end_marker_sent = false;
        let mut next_ready_poll = Instant::now();
        let mut paused = false;
        let mut prepared = false;
        let mut stop_capture = false;
        let mut pause_barrier = None::<u32>;
        let mut pause_request_ns = None::<u128>;
        let mut moment_hold = MomentHold::default();
        let mut pending = Vec::<String>::new();
        let running = Arc::new(AtomicBool::new(true));
        let signal_running = Arc::clone(&running);
        ctrlc::set_handler(move || signal_running.store(false, Ordering::Relaxed))
            .map_err(|error| format!("install interrupt handler: {error}"))?;
        if o.follow_matches { eprintln!("waiting_for_match build={} slot={} start_before=final-match-confirmation", o.build, o.slot); }
        loop {
            if !running.load(Ordering::Relaxed) {
                if o.trace {
                    eprintln!("shutdown signal=SIGINT mailbox_release=begin");
                }
                return Ok(());
            }
            if waiting_ready && Instant::now() >= next_ready_poll {
                next_ready_poll = Instant::now() + Duration::from_millis(20);
                menu = fresh_menu(&o.out, &o.build, o.epoch, o.slot, ready_after_ns, clock_ns(libc::CLOCK_REALTIME).map_err(|e| e.to_string())?)?;
                if let Some((epoch, delay, modified)) = fresh_ready(&o.out, &o.build, o.slot, o.epoch, ready_after_ns)? {
                    ready_publication_ns = control_clock.publication(modified, monotonic_ns().map_err(|e| e.to_string())?)?;
                    o.epoch = epoch;
                    o.delay = delay;
                    next_frame = 1 + delay;
                    segment = FrameSegment { epoch_ns: u128::MAX, first_frame: next_frame };
                    control_sequence = 1;
                    paused = false;
                    prepared = false;
                    pause_barrier = None;
                    stop_capture = false;
                    ended = false;
                    end_marker_sent = false;
                    state = State::default();
                    row_state = state;
                    previous = 0;
                    edges.clear();
                    snapshots.clear();
                    pending.clear();
                    focus_input.armed = false;
                    focus_input.gameplay_armed = false;
                    let sender = mailbox.as_mut().expect("follow-matches editbox sender");
                    sender.epoch = epoch;
                    sender.text_window = TextWindow::default();
                    sender.chat = 0;
                    sender.chat_state = 0;
                    sender.chat_quiet = 0;
                    sender.chat_opened = 0;
                    sender.text_receipt_at = None;
                    clear_quiescent(&o.out, &o.build, o.epoch, o.slot)?;
                    sender.enqueue(format!("JR1{epoch}")).map_err(|e| e.to_string())?;
                    waiting_ready = false;
                    waiting_start = true;
                    eprintln!("match_ready epoch={epoch} neutral_rearm=required");
                }
            }
            // Only seal intervals completed before this queue drain. Taking
            // the cutoff afterwards races events arriving between read and seal.
            let now = monotonic_ns().map_err(|e| e.to_string())?;
            let before_armed = (focus_input.armed, focus_input.gameplay_armed);
            let eligible = mailbox
                .as_mut()
                .map(MailboxSender::focused)
                .transpose()
                .map_err(|error| format!("journal focus: {error}"))?
                .unwrap_or(true) && mailbox.as_ref().is_none_or(|sender| sender.chat_state == 0);
            let lost = mailbox
                .as_mut()
                .is_some_and(|sender| std::mem::take(&mut sender.focus.lost));
            let focus_now = monotonic_ns().map_err(|e| e.to_string())?;
            let changed = focus_input.eligible != eligible;
            if focus_input.observe(eligible, lost, focus_now) && !paused && !stop_capture && !waiting_ready && !waiting_start {
                let frame = release_for_focus_loss(
                    &mut state,
                    &mut edges,
                    &mut snapshots,
                    next_frame,
                    segment,
                    focus_now,
                )?;
                focus_input.accept_since_ns = focus_input.accept_since_ns.max(
                    segment.epoch_ns
                        + (u128::from(frame - segment.first_frame) * 1_000_000_000).div_ceil(HZ),
                );
                eprintln!("focus_release mono_ns={focus_now} frame={frame}");
            }
            if changed {
                eprintln!("game-eligible={eligible} mono_ns={now} neutral_rearm=required");
            }
            if changed || lost || !eligible { recovery.ready = false; }
            menu_input.observe(
                menu.filter(|_| waiting_ready && eligible && !lost && focus_input.armed && device.is_some() && recovery.ready),
                focus_input.physical, start_held, focus_now,
            );
            if device.is_none() && can_reconnect && Instant::now() >= next_reconnect {
                next_reconnect = Instant::now() + Duration::from_millis(100);
                let mut candidates = matching_devices(&identity).map_err(|e| format!("find disconnected controller: {e}"))?;
                if candidates.len() > 1 {
                    if !ambiguous { eprintln!("controller_reconnect_ambiguous matches={} identity={identity:?}", candidates.len()); }
                    ambiguous = true;
                } else if let Some(path) = candidates.pop() {
                    ambiguous = false;
                    let restored = (|| -> io::Result<_> {
                        let replacement = RawDevice::open(&path)?;
                        // The event number may have been reused after discovery.
                        if DeviceIdentity::of(&replacement) != identity { return Ok(None); }
                        set_monotonic_event_clock(&replacement)?;
                        set_nonblocking(&replacement)?;
                        let snapshot = device_snapshot(&replacement)?;
                        Ok(Some((replacement, snapshot)))
                    })();
                    match restored {
                        Ok(Some((replacement, (ranges, physical, start)))) => {
                            let ns = monotonic_ns().map_err(|e| e.to_string())?;
                            capture_axes = ranges;
                            start_held = start;
                            recovery.restored(physical, start, ns);
                            if waiting_ready {
                                axes = ranges;
                                focus_input.restore(physical, start, ns, false);
                            } else {
                                pending_input.push_capture(Capture::Reconnected { ns, axes: ranges, physical, start })?;
                            }
                            device = Some(replacement);
                            eprintln!("controller_reconnected source={} mono_ns={ns} frontier={next_frame} identity={identity:?} neutral_rearm=required", path.display());
                        }
                        Ok(None) => {}
                        Err(error) if error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ENODEV) => {}
                        Err(error) => return Err(format!("reopen controller {}: {error}", path.display())),
                    }
                }
            }
            let read = device.as_mut().map(|device| device.fetch_events().map(|events| events.map(|event| labels.positional(event)).collect::<Vec<_>>()));
            let events = match read {
                Some(Ok(events)) => events,
                None => Vec::new(),
                Some(Err(error)) if error.kind() == io::ErrorKind::WouldBlock => Vec::new(),
                Some(Err(error)) if error.raw_os_error() == Some(libc::ENODEV) => {
                    let ns = monotonic_ns().map_err(|e| e.to_string())?;
                    device = None;
                    recovery.ready = false;
                    start_held = false;
                    menu_input.observe(None, State::default(), false, ns);
                    if waiting_ready {
                        focus_input.restore(State::default(), false, ns, false);
                        focus_input.armed = false;
                    } else {
                        pending_input.push_capture(Capture::Disconnected(ns))?;
                    }
                    eprintln!("controller_disconnected mono_ns={ns} frontier={next_frame} automatic_reconnect={can_reconnect} release_policy=first-unassigned-frame-at-detection");
                    Vec::new()
                }
                Some(Err(error)) => return Err(format!("evdev read: {error}")),
            };
            for event in events {
                // The reopen snapshot owns held state; older queue entries
                // cannot rearm controls from before this observation interval.
                if event_ns(&event)? < recovery.since_ns { continue; }
                let recovery_ready = recovery.ready;
                recovery.observe(&capture_axes, event);
                let start_before = start_held;
                if editbox { moment_hold.observe(&event, event_ns(&event)?); }
                if editbox {
                    if let Some(wire) = pause_request_wire(
                        &event,
                        &mut start_held,
                        o.epoch,
                        control_sequence,
                        paused,
                        prepared
                            || pause_barrier.is_some()
                            || stop_capture
                            || waiting_ready
                            || waiting_start
                            || !eligible
                            || !recovery_ready,
                    ) {
                        mailbox
                            .as_mut()
                            .expect("editbox sender")
                            .enqueue(wire)
                            .map_err(|error| format!("controller pause request: {error}"))?;
                    }
                }
                if waiting_ready {
                    let before = focus_input.physical;
                    focus_input.accepts(&capture_axes, event, start_held, false)?;
                    if let Some(key) = menu_input.press(before, focus_input.physical, start_before, start_held, event_ns(&event)?) {
                        // Recheck the map and exact target for every finite tap.
                        // Directed press/release pairs leave no held menu keys
                        // to leak into gameplay or a newly focused application.
                        let current = fresh_menu(&o.out, &o.build, o.epoch, o.slot, ready_after_ns, clock_ns(libc::CLOCK_REALTIME).map_err(|e| e.to_string())?)?;
                        let sender = mailbox.as_mut().expect("follow-matches editbox sender");
                        if current == menu_input.phase && sender.eligible().map_err(|e| e.to_string())? {
                            sender.typist.text(key).map_err(|e| format!("menu tap: {e}"))?;
                            eprintln!("menu_emit epoch={} phase={:?} key={key} mono_ns={}", o.epoch, current, event_ns(&event)?);
                        } else {
                            menu_input.observe(None, focus_input.physical, start_held, focus_now);
                        }
                    }
                } else {
                    pending_input.push(event, start_before, start_held)?;
                }
            }
            if editbox && !waiting_ready && !waiting_start && !ended {
                if let Some(wire) = moment_hold.request(monotonic_ns().map_err(|e| e.to_string())?, o.epoch) {
                    mailbox.as_mut().expect("editbox sender").enqueue(wire).map_err(|error| format!("controller moment request: {error}"))?;
                }
            }
            let mut start_discard_before_ns = 0;
            if waiting_start {
                let publication = read_control(&lifecycle_path(&o.out, &o.build, o.epoch, o.slot, "start"), control_clock, ready_publication_ns)?;
                start_discard_before_ns = publication.discard_before_ns;
                if let Some(start) = publication.publication {
                    validate_lifecycle(&start.command, &o, ControlState::Started)?;
                    segment = FrameSegment { epoch_ns: start.epoch_ns, first_frame: next_frame };
                    waiting_start = false;
                    eprintln!("match_start epoch={} epoch_ns={} read_ns={} uncertainty_ns={}", o.epoch, start.epoch_ns, start.read_ns, start.uncertainty_ns);
                }
            }
            if !waiting_ready && !ended {
                let publication = read_control(&lifecycle_path(&o.out, &o.build, o.epoch, o.slot, "end"), control_clock, ready_publication_ns)?;
                if let Some(end) = publication.publication {
                    validate_lifecycle(&end.command, &o, ControlState::Ended)?;
                    ended = true;
                    stop_capture = true;
                    paused = false;
                    waiting_start = false;
                    prepared = false;
                    pause_barrier = None;
                    edges.clear();
                    snapshots.clear();
                    pending.clear();
                    ready_after_ns = (end.epoch_ns as i128 + control_clock.offset_ns) as u128;
                    eprintln!("match_end epoch={} frontier={} old_rows=drain terminal_rows=discard", o.epoch, next_frame);
                }
            }
            let command_path = control_path(&o.out, &o.build, o.epoch, o.slot, control_sequence);
            let control_read = if waiting_ready || waiting_start || ended {
                ControlRead { publication: None, discard_before_ns: start_discard_before_ns }
            } else { read_control(&command_path, control_clock, ready_publication_ns)? };
            let command = control_read.publication;
            if let Some(publication) = command.as_ref() {
                let command = &publication.command;
                if command.build != o.build || command.epoch != o.epoch
                    || command.slot != o.slot || command.sequence != control_sequence
                {
                    return Err("journal control identity does not match the active session".into());
                }
            }
            let mut pause_after_seal = None;
            if let Some(publication) = command {
                let command = publication.command;
                if command.state == ControlState::PausePrepare && !paused {
                    pause_request_ns.get_or_insert(
                        publication.epoch_ns.saturating_add(publication.uncertainty_ns),
                    );
                    pause_after_seal = Some(command);
                } else if command.state == ControlState::PauseCommit && paused && prepared {
                    if command.requested_frame < next_frame {
                        return Err(format!(
                            "pause barrier {} precedes helper frontier {next_frame}",
                            command.requested_frame
                        ));
                    }
                    paused = false;
                    prepared = false;
                    pause_request_ns = None;
                    pause_barrier = Some(command.requested_frame);
                    segment = FrameSegment {
                        epoch_ns: now,
                        first_frame: next_frame,
                    };
                    state = State::default();
                    row_state = State::default();
                    previous = 0;
                    edges.clear();
                    snapshots.clear();
                    pending.clear();
                    if o.trace {
                        eprintln!(
                            "control sequence={} state=PAUSE_COMMIT target={} frontier={} epoch_ns={now}",
                            command.sequence, command.requested_frame, next_frame
                        );
                    }
                    control_sequence += 1;
                } else if command.state == ControlState::Resumed && paused && !prepared {
                    if command.requested_frame != next_frame {
                        return Err(format!(
                            "resume requested frame {} does not match paused cursor {next_frame}",
                            command.requested_frame
                        ));
                    }
                    segment = FrameSegment {
                        epoch_ns: publication.epoch_ns,
                        first_frame: next_frame,
                    };
                    state = State::default();
                    row_state = State::default();
                    previous = 0;
                    edges.clear();
                    snapshots.clear();
                    pending.clear();
                    paused = false;
                    focus_input.gameplay_armed = false;
                    submit_control_ack(
                        &o.out,
                        &o.build,
                        o.epoch,
                        o.slot,
                        command.sequence,
                        command.state,
                        next_frame,
                        &mut mailbox,
                    )
                    .map_err(|e| e.to_string())?;
                    if o.trace {
                        eprintln!(
                            "control sequence={} state=RESUME frame={} epoch_ns={} read_ns={} uncertainty_ns={} timestamp_resolution_ns={}",
                            command.sequence, next_frame, publication.epoch_ns, publication.read_ns,
                            publication.uncertainty_ns, control_clock.timestamp_resolution_ns
                        );
                    }
                    control_sequence += 1;
                } else if command.state != ControlState::PauseCommit {
                    return Err("journal pause/resume commands are out of order".into());
                }
            }
            // During PREPARE, drain only events before the publication's
            // uncertainty bound and freeze the output horizon there. This
            // preserves earlier rows while the sender catches up to a finite
            // seal; later input stays queued until the pause boundary is set.
            let output_ready = mailbox.as_ref().is_none_or(|sender| sender.queued.frame_credit());
            let discard_before_ns = pause_request_ns.unwrap_or(control_read.discard_before_ns);
            while let Some(capture) = pending_input.pop_for_output(
                output_ready,
                paused || waiting_start || pause_after_seal.is_some(),
                discard_before_ns,
            )? {
                let (event, start_before, event_start_held) = match capture {
                    Capture::Event(event, before, after) => (event, before, after),
                    Capture::Disconnected(ns) => {
                        if !paused && !stop_capture && !waiting_ready && !waiting_start {
                            let frame = release_for_focus_loss(&mut state, &mut edges, &mut snapshots, next_frame, segment, ns)?;
                            focus_input.accept_since_ns = focus_input.accept_since_ns.max(
                                segment.epoch_ns + (u128::from(frame - segment.first_frame) * 1_000_000_000).div_ceil(HZ),
                            );
                            eprintln!("controller_release mono_ns={ns} frame={frame}");
                        }
                        focus_input.restore(State::default(), false, ns, false);
                        focus_input.armed = false;
                        continue;
                    }
                    Capture::Reconnected { ns, axes: ranges, physical, start } => {
                        axes = ranges;
                        focus_input.restore(physical, start, ns, !paused && !stop_capture && !waiting_ready && !waiting_start);
                        continue;
                    }
                };
                let accepts = focus_input.accepts_in_segment(
                    &axes, event, start_before, event_start_held, !paused && !stop_capture && !waiting_ready && !waiting_start, segment,
                )?;
                if accepts {
                    apply_event(
                        &axes,
                        &mut state,
                        event,
                        &mut edges,
                        &mut snapshots,
                        next_frame,
                        segment,
                        o.trace,
                    )?;
                } else if o.trace
                    && matches!(
                        event.destructure(),
                        EventSummary::Key(..) | EventSummary::AbsoluteAxis(..)
                    )
                {
                    eprintln!(
                        "suppressed mono_ns={} reason=focus-pause-or-neutral",
                        event_ns(&event)?
                    );
                }
            }
            if pending_input.events.is_empty() && device.is_some() && recovery.ready {
                focus_input.rearm(focus_now, start_held, !paused && !stop_capture && !waiting_ready && !waiting_start);
            }
            if device.is_some() && eligible && action_state(recovery.physical) == 0 && !recovery.start {
                recovery.ready = true;
            }
            if before_armed != (focus_input.armed, focus_input.gameplay_armed) {
                eprintln!(
                    "input_armed={} gameplay_armed={} mono_ns={focus_now}",
                    focus_input.armed, focus_input.gameplay_armed
                );
            }
            if !paused && !stop_capture && !waiting_ready && !waiting_start && now >= segment.epoch_ns {
                let mut completed_through = frame_through(segment, now);
                if let (Some(command), Some(published_ns)) = (pause_after_seal.as_ref(), pause_request_ns) {
                    let assigned_through = edges.keys().chain(snapshots.keys()).copied().max().unwrap_or(0);
                    completed_through = completed_through.min(pause_seal_frontier(
                        segment,
                        published_ns,
                        command.requested_frame,
                        assigned_through,
                    ));
                }
                if let Some(barrier) = pause_barrier {
                    completed_through = completed_through.min(barrier - 1);
                }
                while output_ready && next_frame <= completed_through
                    && mailbox.as_ref().is_none_or(|sender| sender.queued.frame_credit())
                {
                    if o.stop_frame.is_some_and(|stop| next_frame > stop) {
                        stop_capture = true;
                        break;
                    }
                    if let Some(frame_state) = snapshots.remove(&next_frame) {
                        row_state = frame_state;
                    }
                    let edge = edges.remove(&next_frame).unwrap_or_default();
                    let row = encode_row(row_state, previous, edge);
                    previous = action_state(row_state);
                    pending.push(row);
                    if pending.len() == 2 {
                        submit_packet(
                            &o.out,
                            &o.build,
                            o.epoch,
                            o.slot,
                            next_frame - 1,
                            &pending,
                            &mut mailbox,
                        )
                        .map_err(|e| e.to_string())?;
                        pending.clear();
                    }
                    if o.trace {
                        eprintln!("published_frame={next_frame}");
                    }
                    if o.stop_frame == Some(next_frame) {
                        if !pending.is_empty() {
                            submit_packet(
                                &o.out,
                                &o.build,
                                o.epoch,
                                o.slot,
                                next_frame,
                                &pending,
                                &mut mailbox,
                            )
                            .map_err(|e| e.to_string())?;
                            pending.clear();
                        }
                        stop_capture = true;
                        break;
                    }
                    next_frame += 1;
                }
                if let Some(command) = pause_after_seal.filter(|_| output_ready && next_frame > completed_through) {
                    if !pending.is_empty() {
                        submit_packet(
                            &o.out,
                            &o.build,
                            o.epoch,
                            o.slot,
                            next_frame - pending.len() as u32,
                            &pending,
                            &mut mailbox,
                        )
                        .map_err(|e| e.to_string())?;
                        pending.clear();
                    }
                    paused = true;
                    prepared = true;
                    pause_request_ns = None;
                    submit_control_ack(
                        &o.out,
                        &o.build,
                        o.epoch,
                        o.slot,
                        command.sequence,
                        ControlState::PausePrepare,
                        next_frame,
                        &mut mailbox,
                    )
                    .map_err(|e| e.to_string())?;
                    if o.trace {
                        eprintln!(
                            "control sequence={} state=PREPARE frame={} epoch_ns={now}",
                            command.sequence, next_frame
                        );
                    }
                    control_sequence += 1;
                }
                if pause_barrier.is_some_and(|barrier| next_frame >= barrier) {
                    let barrier = pause_barrier.take().unwrap();
                    if next_frame != barrier {
                        return Err("pause barrier cursor advanced past its requested frame".into());
                    }
                    if !pending.is_empty() {
                        submit_packet(
                            &o.out,
                            &o.build,
                            o.epoch,
                            o.slot,
                            next_frame - pending.len() as u32,
                            &pending,
                            &mut mailbox,
                        )
                        .map_err(|e| e.to_string())?;
                        pending.clear();
                    }
                    paused = true;
                    submit_control_ack(
                        &o.out,
                        &o.build,
                        o.epoch,
                        o.slot,
                        control_sequence - 1,
                        ControlState::Paused,
                        barrier,
                        &mut mailbox,
                    )
                    .map_err(|e| e.to_string())?;
                    if o.trace {
                        eprintln!(
                            "control sequence={} state=PAUSE frame={barrier} epoch_ns={now}",
                            control_sequence - 1
                        );
                    }
                }
            }
            if ended && !end_marker_sent {
                if let Some(sender) = mailbox.as_mut() {
                    let marker = format!("JE1{}", o.epoch);
                    if sender.queued.records.len() < OUTPUT_RECORD_LIMIT
                        && sender.queued.bytes + marker.len() + 1 <= OUTPUT_BYTE_LIMIT {
                        sender.enqueue(marker).map_err(|e| e.to_string())?;
                        end_marker_sent = true;
                    }
                } else { end_marker_sent = true; }
            }
            if let Some(mailbox) = mailbox.as_mut().filter(|_| !waiting_ready) {
                mailbox
                    .step(paused && !prepared && pause_barrier.is_none())
                    .map_err(|error| format!("keyboard mailbox output: {error}"))?;
            }
            if stop_capture && (!ended || end_marker_sent) && mailbox.as_ref().is_none_or(MailboxSender::is_idle) {
                if ended {
                    publish_symbol(&quiescent_path(&o.out, &o.build, o.epoch, o.slot), b'Q').map_err(|e| e.to_string())?;
                }
                eprintln!("match_quiescent epoch={}", o.epoch);
                if !o.follow_matches { return Ok(()); }
                waiting_ready = true;
                eprintln!("waiting_for_match build={} slot={} after_epoch={}", o.build, o.slot, o.epoch);
                stop_capture = false;
                focus_input.gameplay_armed = false;
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    pub fn main() {
        if let Err(error) = run() {
            eprintln!("wc3-journal: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(target_os = "linux")]
fn main() {
    linux::main();
}
