use std::{fs, path::Path, process::Command};

const GAME_EXTENSIONS: &[&str] = &[
    "mdx", "mdl", "blp", "dds", "slk", "fdf", "toc", "w3x", "w3m", "w3n", "w3g", "mpq", "j", "w3u", "w3t", "w3a", "w3b", "w3d", "w3h", "w3q", "w3e",
    "wpm", "doo", "w3i", "w3r", "w3c", "w3s",
];
const GAME_MAGIC: &[&[u8]] = &[b"MDLX", b"BLP1", b"BLP2", b"MPQ\x1a", b"MPQ\x1b", b"HM3W"];
const MEMORY_ACCESS: &[&str] = &[
    "process_vm_readv",
    "process_vm_writev",
    "ptrace",
    "ReadProcessMemory",
    "WriteProcessMemory",
    "VirtualProtectEx",
    "VirtualAllocEx",
    "CreateRemoteThread",
];
const PRIVATE_TOOLS: &str = "wisp-private";
const GAME_BINARIES: &[&str] = &["Warcraft III.exe", "War3.exe", "Game.dll"];
const SELF: &str = "model/tests/clean_room.rs";

fn proc_mem(text: &str) -> bool {
    text.match_indices("/proc/").any(|(at, _)| {
        let rest = &text[at + "/proc/".len()..];
        let id: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '{' | '}' | '$' | '_')).collect();
        !id.is_empty() && rest[id.len()..].strip_prefix("/mem").is_some_and(|after| !after.starts_with(|c: char| c.is_ascii_alphanumeric()))
    })
}

fn address_table(text: &str) -> bool {
    text.lines().any(|line| {
        GAME_BINARIES.iter().any(|name| line.contains(name))
            && line.match_indices("0x").any(|(at, _)| line[at + 2..].chars().take_while(char::is_ascii_hexdigit).count() >= 6)
    })
}

fn problems(files: &[(&str, &[u8])]) -> Vec<String> {
    let mut found = Vec::new();
    for (path, bytes) in files {
        if *path == SELF {
            continue;
        }
        let extension = path.rsplit_once('.').map(|(_, extension)| extension.to_ascii_lowercase());
        if extension.as_deref().is_some_and(|extension| GAME_EXTENSIONS.contains(&extension)) {
            found.push(format!("{path}: a game-format file"));
        } else if GAME_MAGIC.iter().any(|magic| bytes.starts_with(magic)) {
            found.push(format!("{path}: starts with a game-format signature"));
        }
        let Ok(text) = std::str::from_utf8(bytes) else { continue };
        for api in MEMORY_ACCESS {
            if text.contains(api) {
                found.push(format!("{path}: memory-access API {api}"));
            }
        }
        if proc_mem(text) {
            found.push(format!("{path}: reads or writes another process's /proc/<pid>/mem"));
        }
        if address_table(text) {
            found.push(format!("{path}: an address or offset into the game binary"));
        }
        if !path.ends_with(".md") && text.contains(PRIVATE_TOOLS) {
            found.push(format!("{path}: imports private test tooling"));
        }
    }
    found
}

#[test]
fn tracked_files_respect_blizzards_ip() {
    let refused: &[(&str, &[u8], &str)] = &[
        ("a.rs", b"unsafe { libc::process_vm_readv(pid, a, 1, b, 1, 0) }", "process_vm_readv"),
        ("a.rs", b"libc::ptrace(PTRACE_PEEKDATA, pid, addr, 0)", "ptrace"),
        ("a.py", b"open(f'/proc/{pid}/mem', 'rb')", "/proc/<pid>/mem"),
        ("a.rs", b"File::open(\"/proc/1234/mem\")", "/proc/<pid>/mem"),
        ("a.c", b"ReadProcessMemory(h, p, buf, n, &got)", "ReadProcessMemory"),
        ("a.c", b"WriteProcessMemory(h, p, buf, n, &put)", "WriteProcessMemory"),
        ("a.c", b"VirtualProtectEx(h, p, n, PAGE_EXECUTE_READWRITE, &old)", "VirtualProtectEx"),
        ("a.c", b"CreateRemoteThread(h, 0, 0, f, 0, 0, 0)", "CreateRemoteThread"),
        ("notes.txt", b"Warcraft III.exe base + 0x00A1B2C3 is the unit table", "address table"),
        ("a.ts", b"import { patch } from \"~/.local/share/wisp-private/lan/memory\"", "wisp-private"),
        ("Footman.mdx", b"anything", "extension"),
        ("ui.fdf", b"Frame", "extension"),
        ("blob.bin", b"BLP1\0\0\0\0", "magic"),
        ("blob.dat", b"HM3W\0\0\0\0", "magic"),
    ];
    for (path, bytes, what) in refused {
        assert!(!problems(&[(path, bytes)]).is_empty(), "{what} in {path} must be refused");
    }
    let allowed: &[(&str, &[u8])] = &[
        ("src/pad.rs", b"fs::read_to_string(format!(\"/proc/{pid}/stat\"))"),
        ("src/pad.rs", b"let status = \"/proc/self/status\";"),
        ("README.md", b"Private tools live under ~/.local/share/wisp-private/."),
        ("src/focus.rs", b"name.eq_ignore_ascii_case(\"Warcraft III.exe\")"),
        ("src/stick.rs", b"const THRESHOLD: u32 =;"),
    ];
    for (path, bytes) in allowed {
        assert_eq!(problems(&[(path, bytes)]), Vec::<String>::new(), "{path} is ours and allowed");
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("the workspace root");
    let listing = Command::new("git").current_dir(root).args(["ls-files", "-z"]).output().expect("git ls-files");
    assert!(listing.status.success(), "git ls-files failed");
    let tracked: Vec<(String, Vec<u8>)> = listing
        .stdout
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| {
            let name = String::from_utf8(name.to_vec()).expect("a utf-8 file name");
            let bytes = fs::read(root.join(&name)).unwrap_or_default();
            (name, bytes)
        })
        .collect();
    assert!(tracked.len() > 10, "git ls-files returned {} files", tracked.len());
    let views: Vec<(&str, &[u8])> = tracked.iter().map(|(name, bytes)| (name.as_str(), bytes.as_slice())).collect();
    assert_eq!(problems(&views), Vec::<String>::new(), "see the README, \"Respecting Blizzard's IP\"");
}
