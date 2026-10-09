//! Map plug-ins in another program. The service runs `PROGRAM --plugin` and
//! talks to it in JSON, one object per line: the program first writes a
//! [`Hello`], then answers every [`Request`] on its standard input with one
//! reply line on its standard output (README, "Map plug-ins"). [`serve`] is
//! that program's side for a [`Profile`] written in Rust.

use super::{Game, Helper, Line, Pad, Profile, Session};
use serde::{Deserialize, Serialize};
use std::{
    io::{self, BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

/// The plug-in's first line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub name: String,
    pub title: String,
}

/// From the service; each gets exactly one reply line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Request {
    /// Reply: [`SessionReply`].
    Session(Game),
    /// Reply: a [`Helper`] or `null`.
    Helper { game: Game, pad: Pad, session: Session },
    /// A line the helper wrote. Reply: [`Line`].
    Line(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionReply {
    pub session: Option<Session>,
    /// The session plays on keys: the service presses the fighter layout and runs no helper.
    pub keys: bool,
    /// The map shows a menu the pad drives with the desktop pointer now.
    pub pointer_menu: bool,
}

/// Answers the service's requests with `profile` until its input ends.
pub fn serve(profile: &mut dyn Profile, input: impl BufRead, mut output: impl Write) -> io::Result<()> {
    send(&mut output, &Hello { name: profile.name().into(), title: profile.title().into() })?;
    for line in input.lines() {
        let request: Request = serde_json::from_str(&line?).map_err(io::Error::other)?;
        match request {
            Request::Session(game) => {
                let session = profile.session(&game);
                send(&mut output, &SessionReply { session, keys: profile.keys(), pointer_menu: profile.pointer_menu() })?;
            }
            Request::Helper { game, pad, session } => send(&mut output, &profile.helper(&game, &pad, &session))?,
            Request::Line(line) => send(&mut output, &profile.line(&line))?,
        }
    }
    Ok(())
}

fn send(output: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
    writeln!(output, "{}", serde_json::to_string(value).map_err(io::Error::other)?)?;
    output.flush()
}

/// A plug-in program the service talks to. A plug-in that fails is reported
/// once and then follows no session.
pub struct Plugin {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    hello: Hello,
    last: SessionReply,
    failed: bool,
}

impl Plugin {
    pub fn spawn(program: &Path) -> Result<Self, String> {
        let mut child = Command::new(program).arg("--plugin").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()
            .map_err(|error| format!("start the map plug-in {}: {error}", program.display()))?;
        let input = child.stdin.take().expect("piped plug-in input");
        let mut output = BufReader::new(child.stdout.take().expect("piped plug-in output"));
        let mut line = String::new();
        output.read_line(&mut line).map_err(|error| format!("read the map plug-in's hello: {error}"))?;
        let hello = serde_json::from_str(&line).map_err(|error| format!("the map plug-in {} said {line:?}: {error}", program.display()))?;
        Ok(Self { child, input, output, hello, last: SessionReply::default(), failed: false })
    }

    fn ask<T: for<'de> Deserialize<'de> + Default>(&mut self, request: &Request) -> T {
        if self.failed {
            return T::default();
        }
        let reply = (|| -> Result<T, String> {
            let text = serde_json::to_string(request).map_err(|error| error.to_string())?;
            writeln!(self.input, "{text}").and_then(|()| self.input.flush()).map_err(|error| error.to_string())?;
            let mut line = String::new();
            if self.output.read_line(&mut line).map_err(|error| error.to_string())? == 0 {
                return Err("it exited".into());
            }
            serde_json::from_str(&line).map_err(|error| format!("it said {line:?}: {error}"))
        })();
        reply.unwrap_or_else(|error| {
            eprintln!("service: the map plug-in {} failed: {error}", self.hello.name);
            self.failed = true;
            T::default()
        })
    }
}

impl Drop for Plugin {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Profile for Plugin {
    fn name(&self) -> &str {
        &self.hello.name
    }
    fn title(&self) -> &str {
        &self.hello.title
    }
    fn session(&mut self, game: &Game) -> Option<Session> {
        self.last = self.ask(&Request::Session(game.clone()));
        self.last.session.clone()
    }
    fn helper(&mut self, game: &Game, pad: &Pad, session: &Session) -> Option<Helper> {
        self.ask(&Request::Helper { game: game.clone(), pad: pad.clone(), session: session.clone() })
    }
    fn line(&mut self, line: &str) -> Line {
        self.ask(&Request::Line(line.into()))
    }
    fn pointer_menu(&mut self) -> bool {
        self.last.pointer_menu
    }
    fn keys(&mut self) -> bool {
        self.last.keys
    }
}
