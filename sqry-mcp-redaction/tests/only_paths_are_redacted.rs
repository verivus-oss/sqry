//! Redaction changes only values that are paths (round 7, surfaces round
//! three, finding 1), and every value that carries a path is redacted
//! (round 7, surfaces round four, finding 1).
//!
//! `source`, `target`, `src`, `dst`, `uri` and `url` are path keys in some
//! envelopes and not in others: sqry's own responses put an enumerated value
//! under `source` (`query_too_broad` says `static_estimate` or
//! `runtime_budget`, classpath provenance says `classpath`) and a symbol name
//! under `source` and `target` (`direct_callees`, `direct_callers`,
//! `semantic_diff` edges). Round three stopped rewriting those, but it read
//! a value as a path only when it was an absolute path, so `file:/x`,
//! `FILE:///x`, prose holding a path, a leading space, `~/x`, a relative
//! path and `C:proj\lib.rs` passed unchanged. A value under these keys now
//! carries a path when it holds a path separator and is not one URL of
//! another scheme (`classify_contextual_value`).
//!
//! Each test drives every redacting preset, with no workspace, with a
//! workspace root and with a bound logical workspace, from both sides: the
//! values that are not paths pass unchanged, and the values that carry a
//! path are rewritten.

use std::path::PathBuf;

use serde_json::{Value, json};
use sqry_mcp_redaction::rules::path::{
    ContextualPath, classify_contextual_value, is_absolute_path_text,
};
use sqry_mcp_redaction::{LogicalWorkspaceView, RedactionConfig, Redactor, compute_source_root_id};

const ROOT: &str = "/home/user/proj";

/// A redaction preset's constructor.
type Preset = fn() -> RedactionConfig;

/// Every redacting preset, each with no workspace, with a workspace root,
/// and with a logical workspace bound to the same root.
fn redactors() -> Vec<(String, Redactor)> {
    let presets: [(&str, Preset); 4] = [
        ("minimal", RedactionConfig::minimal),
        ("relative", RedactionConfig::relative),
        ("standard", RedactionConfig::standard),
        ("strict", RedactionConfig::strict),
    ];
    let mut out = Vec::new();
    for (name, preset) in presets {
        out.push((
            format!("{name}/no workspace"),
            Redactor::new(preset()).expect("redactor"),
        ));
        let mut rooted = preset();
        rooted.workspace_root = Some(PathBuf::from(ROOT));
        out.push((
            format!("{name}/workspace root"),
            Redactor::new(rooted.clone()).expect("redactor"),
        ));
        let id = compute_source_root_id("0123456789abcdef", std::path::Path::new(ROOT));
        let view = LogicalWorkspaceView {
            workspace_id_short: "0123456789abcdef".to_string(),
            source_roots: vec![(id, PathBuf::from(ROOT))],
            member_folders: Vec::new(),
            exclusions: Vec::new(),
        };
        out.push((
            format!("{name}/logical workspace"),
            Redactor::with_logical_workspace(rooted, view).expect("redactor"),
        ));
    }
    out
}

/// The values that are not paths, under every contextual key, as sqry
/// sends them: enumerated values, symbol names (a one-letter first segment
/// included, which is not a drive), URLs of other schemes, a drive letter
/// with no separator, and values of every non-string type.
fn not_paths() -> Value {
    json!({
        "details": {
            "source": "static_estimate",
            "kind": "query_too_broad",
            "doc_url": "https://docs.verivus.dev/sqry/query-cost-gate"
        },
        "runtime": { "source": "runtime_budget" },
        "provenance": { "source": "classpath" },
        "callees": { "source": "alpha" },
        "callers": { "target": "beta" },
        "edge": { "source": "S::m", "target": "a::b" },
        "other": {
            "src": "main",
            "dst": "C:cache",
            "uri": "https://example.com/a/b",
            "url": "http://example.com/x"
        },
        "names": {
            "source": "crate::foo",
            "target": "a.b.c",
            "src": "S::~S",
            "dst": "operator+",
            "uri": "git+ssh://host/org/repo.git",
            "url": "~"
        },
        "empty": { "source": "", "target": "   " },
        "typed": {
            "source": 7,
            "target": null,
            "src": true,
            "path": null,
            "root": 3,
            "file_path": false,
            "filePath": ""
        }
    })
}

#[test]
fn values_that_are_not_paths_pass_unchanged() {
    for (name, redactor) in redactors() {
        let mut value = not_paths();
        redactor.redact(&mut value);
        assert_eq!(
            value,
            not_paths(),
            "{name} changed a value that is not a path"
        );
    }
}

/// The `none` preset rewrites nothing when no exclusion is bound: not a
/// value that is not a path, and not a value that carries one, with no
/// workspace, with a workspace root, or with a logical workspace bound.
/// (Its exclusions are pinned in `passthrough_exclusion_paths.rs`.)
#[test]
fn the_none_preset_rewrites_nothing_without_an_exclusion() {
    let mut rooted = RedactionConfig::none();
    rooted.workspace_root = Some(PathBuf::from(ROOT));
    let id = compute_source_root_id("0123456789abcdef", std::path::Path::new(ROOT));
    let view = LogicalWorkspaceView {
        workspace_id_short: "0123456789abcdef".to_string(),
        source_roots: vec![(id, PathBuf::from(ROOT))],
        member_folders: Vec::new(),
        exclusions: Vec::new(),
    };
    let redactors = [
        Redactor::new(RedactionConfig::none()).expect("none"),
        Redactor::new(rooted.clone()).expect("none, rooted"),
        Redactor::with_logical_workspace(rooted, view).expect("none, logical"),
    ];
    for redactor in redactors {
        let mut value = not_paths();
        redactor.redact(&mut value);
        assert_eq!(value, not_paths());
        for key in CONTEXTUAL_KEYS {
            for (input, _) in CARRY_A_PATH {
                let mut value = json!({ key: input });
                redactor.redact(&mut value);
                assert_eq!(value[key], input, "none rewrote {key} {input:?}");
            }
        }
    }
}

/// The absolute paths, under every contextual key and in every form, are
/// still redacted: the absolute text is gone from each.
#[test]
fn absolute_paths_under_contextual_keys_are_still_redacted() {
    let cases = [
        ("source", "/home/user/proj/src/lib.rs"),
        ("target", "file:///home/user/proj/src/main.rs"),
        ("src", "/etc/secret/config"),
        ("dst", "C:\\Users\\user\\proj\\x.rs"),
        ("uri", "file:///srv/other/y.rs"),
        ("url", "\\\\server\\share\\z.rs"),
        ("source", "C:/Users/user/proj/w.rs"),
    ];
    for (name, redactor) in redactors() {
        for (key, path) in cases {
            let mut value = json!({ key: path });
            redactor.redact(&mut value);
            let redacted = value[key].as_str().expect("still a string");
            assert_ne!(redacted, path, "{name}: {key} kept {path}");
            for segment in [
                "/home/user",
                "C:\\Users",
                "C:/Users",
                "/srv/other",
                "/etc/secret",
                "\\\\server",
            ] {
                assert!(
                    !redacted.contains(segment),
                    "{name}: {key} {path} -> {redacted}"
                );
            }
        }
    }
}

/// A key that always names a path keeps its redaction for a relative
/// value: the strict preset still hashes `file_path: "src/lib.rs"`, and an
/// absolute `path` or `root` is still rewritten by every preset.
#[test]
fn keys_that_always_name_a_path_still_redact_relative_and_absolute_values() {
    for (name, redactor) in redactors() {
        let mut value = json!({
            "path": "/home/user/proj/src",
            "root": "/home/user/proj",
            "fileUri": "file:///home/user/proj/src/lib.rs"
        });
        redactor.redact(&mut value);
        for key in ["path", "root", "fileUri"] {
            let redacted = value[key].as_str().expect("string");
            assert!(
                !redacted.contains("/home/user"),
                "{name}: {key} -> {redacted}"
            );
        }
    }
    let strict = Redactor::new(RedactionConfig::strict()).expect("strict");
    let mut relative = json!({ "file_path": "src/lib.rs" });
    strict.redact(&mut relative);
    let hashed = relative["file_path"].as_str().expect("string");
    assert!(
        hashed != "src/lib.rs" && hashed.contains('['),
        "strict hashes a relative file path: {hashed}"
    );
}

/// The contextual keys.
const CONTEXTUAL_KEYS: [&str; 6] = ["source", "target", "src", "dst", "uri", "url"];

/// Every form of value that carries a path outside the workspace, with what
/// the presets that do not hash render it as. Under strict each is a hash.
/// The first eight are the forms round three let through unchanged.
const CARRY_A_PATH: [(&str, &str); 15] = [
    ("file:/mnt/data/proj/lib.rs", "<external>/lib.rs"),
    ("FILE:///mnt/data/proj/lib.rs", "<external>/lib.rs"),
    ("read from /mnt/data/proj/lib.rs", "<external>/lib.rs"),
    (" /mnt/data/proj/lib.rs", "<external>/lib.rs"),
    ("~/secret/proj/src/lib.rs", "<external>/lib.rs"),
    ("src/secret_module/lib.rs", "<external>/lib.rs"),
    ("C:proj\\lib.rs", "<external>/lib.rs"),
    ("at C:/Users/u/proj", "<external>/proj"),
    ("/api/users", "<external>/users"),
    ("file://localhost/mnt/data/x.rs", "<external>/x.rs"),
    ("path:/mnt/data/x.rs", "<external>/x.rs"),
    ("(/mnt/data/x.rs)", "<external>/x.rs)"),
    ("/mnt/My Documents/x.rs", "<external>/x.rs"),
    ("/home/user/proj/a.rs and /etc/passwd", "<external>/passwd"),
    ("../../etc/shadow", "<external>/shadow"),
];

/// Every value that carries a path is rewritten under every contextual key,
/// by every preset, with or without a workspace: only the text after its
/// last separator survives (a hash under strict), so no directory of it is
/// emitted, and a value outside the workspace is never placed inside it.
#[test]
fn every_value_that_carries_a_path_is_redacted_under_every_contextual_key() {
    for (name, redactor) in redactors() {
        for key in CONTEXTUAL_KEYS {
            for (input, expected) in CARRY_A_PATH {
                let mut value = json!({ key: input });
                redactor.redact(&mut value);
                let redacted = value[key].as_str().expect("still a string");
                if name.starts_with("strict") {
                    assert!(
                        redacted.starts_with("<external>/[") && redacted.ends_with(']'),
                        "{name}: {key} {input:?} -> {redacted}"
                    );
                } else {
                    assert_eq!(redacted, expected, "{name}: {key} {input:?}");
                }
            }
        }
    }
}

/// A list under a path key is a list of that key's values: each string in
/// it, through any depth of nested lists, is redacted as the key's own
/// string would be, and an object in it is read by its own keys.
#[test]
fn a_list_under_a_path_key_is_redacted_item_by_item() {
    for (name, redactor) in redactors() {
        let mut value = json!({
            "source": [
                "/mnt/data/x/a.rs",
                "alpha",
                ["read from /mnt/data/y/b.rs", ["~/z/c.rs"]],
                { "name": "data/q", "path": "/mnt/data/p/d.rs" }
            ],
            "path": ["/mnt/data/e.rs", ""]
        });
        redactor.redact(&mut value);
        let items = [
            (&value["source"][0], "<external>/a.rs"),
            (&value["source"][2][0], "<external>/b.rs"),
            (&value["source"][2][1][0], "<external>/c.rs"),
            (&value["source"][3]["path"], "<external>/d.rs"),
            (&value["path"][0], "<external>/e.rs"),
        ];
        for (item, expected) in items {
            let item = item.as_str().expect("string");
            if name.starts_with("strict") {
                assert!(item.starts_with("<external>/["), "{name}: {item}");
            } else {
                assert_eq!(item, expected, "{name}");
            }
        }
        assert_eq!(value["source"][1], "alpha", "{name}");
        assert_eq!(value["path"][1], "", "{name}");
        // The object's own key, not the list's, decides: `name` is no path
        // key, so the relative path under it, which `source` would redact,
        // passes. (An absolute path under `name` is redacted by in-string
        // detection, as under any key, so it cannot show which key decided.)
        assert_eq!(value["source"][3]["name"], "data/q", "{name}");
    }
}

/// A contextual value that is exactly one absolute path, in any of its
/// forms (any `file:` URI form, any letter case, surrounding whitespace),
/// is placed as a path key's value is: inside the workspace it renders
/// exactly as `path: "/home/user/proj/src/lib.rs"` does.
#[test]
fn one_absolute_path_under_a_contextual_key_is_placed_like_a_path_key_value() {
    let forms = [
        "/home/user/proj/src/lib.rs",
        "  /home/user/proj/src/lib.rs ",
        "file:///home/user/proj/src/lib.rs",
        "FILE:///home/user/proj/src/lib.rs",
        "File:/home/user/proj/src/lib.rs",
        "file://localhost/home/user/proj/src/lib.rs",
        "file://LOCALHOST/home/user/proj/src/lib.rs",
    ];
    for (name, redactor) in redactors() {
        let mut reference = json!({ "path": "/home/user/proj/src/lib.rs" });
        redactor.redact(&mut reference);
        let reference = reference["path"].as_str().expect("string").to_string();
        assert!(!reference.contains("/home/user"), "{name}: {reference}");
        for key in CONTEXTUAL_KEYS {
            for form in forms {
                let mut value = json!({ key: form });
                redactor.redact(&mut value);
                assert_eq!(value[key], reference.as_str(), "{name}: {key} {form:?}");
            }
        }
    }
}

/// A key that always names a path reads every `file:` URI form as the path
/// it names: `FILE:///x`, `file:/x` and `file://localhost/x` render as
/// `file:///x` does, inside the workspace and outside it.
#[test]
fn path_keys_read_every_file_uri_form_as_the_path_it_names() {
    for (name, redactor) in redactors() {
        for path in ["/home/user/proj/src/lib.rs", "/mnt/data/proj/lib.rs"] {
            let mut reference = json!({ "fileUri": format!("file://{path}") });
            redactor.redact(&mut reference);
            let reference = reference["fileUri"].as_str().expect("string").to_string();
            assert!(!reference.contains("/mnt"), "{name}: {reference}");
            assert!(!reference.contains("/home/user"), "{name}: {reference}");
            for form in [
                format!("FILE://{path}"),
                format!("file:{path}"),
                format!("file://localhost{path}"),
            ] {
                for key in ["fileUri", "file_uri", "path"] {
                    let mut value = json!({ key: form.as_str() });
                    redactor.redact(&mut value);
                    assert_eq!(value[key], reference.as_str(), "{name}: {key} {form}");
                }
            }
        }
    }
}

/// A URL path under `url` (`/api/users`) cannot be told from a file path,
/// so it is redacted as one, while a URL with its scheme is not read as a
/// path: it is scanned as any other string is, and in-string detection
/// keeps the own path of a URL of a scheme other than `file`
/// (`https://example.com/home/u/x` names no file on this host).
#[test]
fn a_url_path_is_redacted_and_a_url_with_its_scheme_is_left_whole() {
    for (name, redactor) in redactors() {
        let mut value = json!({
            "url": "/api/users",
            "uri": "https://example.com/api/users",
            "edge": { "url": "https://example.com/home/u/x" }
        });
        redactor.redact(&mut value);
        let path = value["url"].as_str().expect("string");
        assert!(!path.contains("/api"), "{name}: {path}");
        assert_eq!(value["uri"], "https://example.com/api/users", "{name}");
        assert_eq!(
            value["edge"]["url"], "https://example.com/home/u/x",
            "{name}"
        );
    }
}

/// Round 8e: through the redactor, a `Debug`-formatted error that holds a
/// Windows path after a newline, a UNC path and a mixed-separator Unix
/// path, and a JSON-escaped Unix path, leaves no directory, user or server
/// readable under any redacting preset. At round 8d the drive path and the
/// JSON path passed whole, the UNC path left `\\\\srv`, and the Unix path
/// left `\alice\secret.txt`.
#[test]
fn escaped_windows_unc_and_json_paths_are_redacted_whole() {
    let message = format!(
        "{:?}",
        "line\nC:\\Users\\alice\\secret and \\\\srv\\share\\x and /mnt/c/Users\\bob\\y.txt"
    );
    let json_text = r#"{"path":"\/home\/carol\/z"}"#;
    for (name, preset) in [
        ("minimal", RedactionConfig::minimal as Preset),
        ("relative", RedactionConfig::relative),
        ("standard", RedactionConfig::standard),
        ("strict", RedactionConfig::strict),
    ] {
        let redactor = Redactor::new(preset()).expect("redactor");
        let mut value = json!({ "message": message, "note": json_text });
        redactor.redact(&mut value);
        let text = value.to_string();
        for leaked in ["alice", "srv", "bob", "carol", "Users", "share"] {
            assert!(!text.contains(leaked), "{name}: {leaked} in {text}");
        }
    }
}

/// Round 8d: through the redactor, a `Debug`-formatted error string keeps
/// its escapes: the path inside `\"...\"` is redacted and the closing `\"`
/// survives (it was `<external>/s"`, the backslash taken into the path), a
/// path after a written-out `\0`, `\f`, `\v`, `\x0a` or `\u000a` is
/// redacted, and a `~=` literal followed by `./` hides nothing.
#[test]
fn a_debug_formatted_error_keeps_its_escapes_and_loses_its_paths() {
    let message = format!(
        "{:?}",
        "open \"/home/u/s\" failed\nat /mnt/t\u{0}/root/a\u{c}/data/b\u{b}/nix/c"
    );
    let extra = "; a\\x0a/home/u/d; a\\u000a/home/u/e; name~=/x/./home/u/f";
    for (name, preset) in [
        ("minimal", RedactionConfig::minimal as Preset),
        ("relative", RedactionConfig::relative),
        ("standard", RedactionConfig::standard),
        ("strict", RedactionConfig::strict),
    ] {
        let redactor = Redactor::new(preset()).expect("redactor");
        let text = format!("{message}{extra}");
        let mut value = json!({ "message": text });
        redactor.redact(&mut value);
        let redacted = value["message"].as_str().expect("message");
        for leaked in ["/home", "/mnt", "/root", "/data", "/nix"] {
            assert!(!redacted.contains(leaked), "{name}: {leaked} in {redacted}");
        }
        assert!(redacted.contains("\\\" failed\\nat "), "{name}: {redacted}");
        assert!(redacted.starts_with("\"open \\\""), "{name}: {redacted}");
    }
}

/// Round 8c: through the redactor, a JSON pointer written as a string in a
/// code snippet (`"$ref": "#/definitions/X"`, under `code` and
/// `context.lines`) is kept under the presets that keep code (`minimal`,
/// `relative`); at round 8b it became `"#<external>/X"`. In message text, a
/// host path after `#`, after a written-out `\n`, after an ellipsis or a
/// `~` that follows a word, after a `::` that is not sqry's endpoint shape,
/// and after an unterminated `~=` literal is redacted under every redacting
/// preset; each passed unredacted at round 8b.
#[test]
fn code_keeps_json_pointers_and_text_paths_after_escapes_are_redacted() {
    let snippet = r##""$ref": "#/definitions/X""##;
    let message = concat!(
        "see #/home/u/a; Error(\"line\\n/home/u/b\"); see.../home/u/c; ",
        "x~/home/u/d; Foo::BAR::/home/u/e; regex name~=/foo failed reading /home/u/f"
    );
    let presets: [(&str, Preset, bool); 4] = [
        ("minimal", RedactionConfig::minimal, true),
        ("relative", RedactionConfig::relative, true),
        ("standard", RedactionConfig::standard, false),
        ("strict", RedactionConfig::strict, false),
    ];
    for (name, preset, keeps_code) in presets {
        let redactor = Redactor::new(preset()).expect("redactor");
        let mut value = json!({
            "code": snippet,
            "context": { "lines": [snippet] },
            "message": message
        });
        redactor.redact(&mut value);
        if keeps_code {
            assert_eq!(value["code"], json!(snippet), "{name}: {value}");
            assert_eq!(
                value["context"]["lines"][0],
                json!(snippet),
                "{name}: {value}"
            );
        }
        assert!(
            !value.to_string().contains("<external>/X"),
            "{name}: {value}"
        );
        let redacted = value["message"].as_str().expect("message");
        assert!(!redacted.contains("/home"), "{name}: {redacted}");
    }
}

/// Round 8b: through the redactor, under every redacting preset, a path in
/// a message after a shell redirection, an operator character or `::`, in
/// a URL's query or parameters, or holding a bracket it opens, is redacted
/// whole; sqry's own regex literals after `~=` (`name~=/.*/`) and a URL's
/// own path are kept exactly. Before, the paths below passed unredacted
/// (or split around the bracket) and every regex literal whose pattern
/// opened with a name character, `.` or `_` was rewritten.
#[test]
fn a_path_after_an_operator_is_redacted_and_a_sqry_regex_is_kept() {
    let paths = [
        "2>/dev/shm/secret/log",
        "cmd >/srv/u/out.txt",
        "cat </mnt/u/in",
        "a&/home/u/x",
        "x+/root/u/y",
        "%/nix/x",
        "$/data/x",
        "*/home/u/x",
        "@/home/u/x",
        "path::/home/u/y",
        "http://h/api;cache=/home/u/.cache",
        "ws://h/a?f=/home/u/z",
        "/home/u/My(1)/sub/file",
        "-I/usr/include",
    ];
    let kept = [
        "predicate `name~=/.*/` is too broad",
        "name~=/foo/i",
        "name~= /test_.*/ms",
        "'name~=/handler/'",
        "https://host/home/u/x",
        "route::GET::/api/users",
        "include/c++/v1",
        "</div>",
    ];
    let presets: [(&str, Preset); 4] = [
        ("minimal", RedactionConfig::minimal),
        ("relative", RedactionConfig::relative),
        ("standard", RedactionConfig::standard),
        ("strict", RedactionConfig::strict),
    ];
    for (name, preset) in presets {
        let redactor = Redactor::new(preset()).expect("redactor");
        for text in paths {
            let mut value = json!({ "message": text });
            redactor.redact(&mut value);
            let message = value["message"].as_str().expect("message");
            for leaked in [
                "/dev/shm", "/srv", "/mnt", "/home", "/root", "/nix", "/data", "/usr", "My(1)",
            ] {
                assert!(!message.contains(leaked), "{name} {text:?}: {message}");
            }
        }
        for text in kept {
            let mut value = json!({ "message": text });
            redactor.redact(&mut value);
            assert_eq!(value["message"], json!(text), "{name}");
        }
    }
}

/// Round 8, B3: under every redacting preset, an absolute path in message
/// text or under a key outside the path-key lists is redacted whatever
/// directory it starts in, not only under `/home`, `/Users`, `/var`,
/// `/srv`, `/opt`, `/tmp` and `/etc`. The workspace root is an explicit
/// path outside those seven, so the result does not depend on where the
/// host keeps its temporary files; with no workspace bound, the root is
/// redacted as any other path.
#[test]
fn every_absolute_path_in_text_is_redacted_whatever_its_prefix() {
    const WS: &str = "/dev/shm/sqry-r8/ws";
    let outside = [
        "/dev/shm/sqry-r8/other/cache",
        "/mnt/data/cache",
        "/nix/store/abc123-pkg/lib",
        "/root/.ssh/id_ed25519",
        "/data/x",
        "C:\\Users\\me\\proj\\a.rs",
        "c:/proj/a.rs",
        "\\\\server\\share\\x",
        "file:///mnt/x",
    ];
    let presets: [(&str, Preset); 4] = [
        ("minimal", RedactionConfig::minimal),
        ("relative", RedactionConfig::relative),
        ("standard", RedactionConfig::standard),
        ("strict", RedactionConfig::strict),
    ];
    let mut checked = 0;
    for (name, preset) in presets {
        for bound in [false, true] {
            let mut config = preset();
            if bound {
                config.workspace_root = Some(PathBuf::from(WS));
            }
            let redactor = Redactor::new(config).expect("redactor");
            let message = format!("rebuild of {WS} refused: {}", outside.join(" and "));
            let mut value = json!({
                "message": message,
                "details": {
                    "expand_cache_dir": outside[0],
                    "manifest_path": format!("{WS}/.sqry/graph/manifest.json"),
                    "reason": format!("cannot read {}", outside[3]),
                    "list": [outside[1], outside[5]]
                }
            });
            redactor.redact(&mut value);
            let text = value.to_string();
            for leaked in [
                "/dev/shm", "/mnt", "/nix", "/root", "/data", "Users", "c:/proj", "server",
            ] {
                assert!(
                    !text.contains(leaked),
                    "{name} bound={bound}: {leaked} in {text}"
                );
            }
            let message = value["message"].as_str().expect("message");
            if bound && name != "strict" {
                assert!(
                    message.starts_with("rebuild of <workspace> refused: "),
                    "{name}: {message}"
                );
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 8);
}

#[test]
fn absolute_path_text_names_the_absolute_forms_only() {
    for path in [
        "/",
        "/home/user/x",
        "\\Windows\\x",
        "\\\\server\\share\\x",
        "\\\\?\\C:\\x",
        "//server/share/x",
        "C:\\x",
        "c:/x",
        "file:///home/user/x",
        "file://server/share/x",
        "FILE:///home/user/x",
        "file:/home/user/x",
        "file://localhost/home/user/x",
    ] {
        assert!(is_absolute_path_text(path), "{path} is absolute");
    }
    for text in [
        "",
        "static_estimate",
        "runtime_budget",
        "classpath",
        "alpha",
        "S::m",
        "a::b",
        "C:cache",
        "src/lib.rs",
        "./x",
        "../x",
        "https://example.com/x",
        "1:/x",
        "file:x",
        "~/x",
        " /home/user/x",
    ] {
        assert!(!is_absolute_path_text(text), "{text} is not absolute");
    }
}

/// The contextual rule, both ways: no separator, or one URL of another
/// scheme, is not a path; exactly one absolute path is anchored (trimmed);
/// any other value with a separator carries a path that names no place.
#[test]
fn contextual_values_are_classified_by_their_separators() {
    for text in [
        "",
        "   ",
        "alpha",
        "crate::foo",
        "a.b.c",
        "S::~S",
        "static_estimate",
        "runtime_budget",
        "classpath",
        "C:cache",
        "~",
        "https://example.com/a/b",
        "HTTPS://example.com/a",
        "git+ssh://host/org/repo",
        "s3://bucket/key",
    ] {
        assert_eq!(
            classify_contextual_value(text),
            ContextualPath::NotAPath,
            "{text:?}"
        );
    }
    for (text, trimmed) in [
        ("/mnt/x", "/mnt/x"),
        (" /mnt/x\t", "/mnt/x"),
        ("\\Windows\\x", "\\Windows\\x"),
        ("C:\\x", "C:\\x"),
        ("c:/x", "c:/x"),
        ("\\\\server\\share", "\\\\server\\share"),
        ("file:/x", "file:/x"),
        ("FILE:///x", "FILE:///x"),
        ("/api/users", "/api/users"),
        ("/", "/"),
        // A one-letter scheme is a drive letter: `X:` then `//y`.
        ("x://y", "x://y"),
    ] {
        assert_eq!(
            classify_contextual_value(text),
            ContextualPath::Anchored(trimmed),
            "{text:?}"
        );
    }
    for text in [
        "src/lib.rs",
        "./x",
        "../x",
        "~/x",
        "C:proj\\lib.rs",
        "path:/x",
        "(/x)",
        "read from /x",
        "/a b/c",
        "at C:/x",
        "see https://example.com/a",
        "file:x/y",
        "github.com/org/pkg",
        "route::GET::/api/users",
        "operator/",
        "App\\Http\\Controller",
    ] {
        assert_eq!(
            classify_contextual_value(text),
            ContextualPath::Unanchored,
            "{text:?}"
        );
    }
}
