//! The gate for `docs/assets/logo/logo.svg`, which www.livid.work shows on
//! thirdeye's card. www ignores a logo that breaks its rules, with a warning
//! in another repository's run, so the rules are held here: at most 2 MB, no
//! script, `foreignObject`, event handler or `javascript:` link, and nothing
//! loaded from outside the file.

use std::path::{Path, PathBuf};

const LOGO: &str = "docs/assets/logo/logo.svg";
const MAX_BYTES: usize = 2 * 1024 * 1024;

fn logo_text() -> String {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask sits in the repository root")
        .join(LOGO);
    std::fs::read_to_string(path).expect("the logo exists")
}

fn problems(svg: &str) -> Vec<String> {
    let mut found = Vec::new();
    if svg.len() > MAX_BYTES {
        found.push("over 2 MB".into());
    }
    let lower = svg.to_lowercase();
    if !lower.contains("viewbox=\"0 0 1024 1024\"") {
        found.push("not a square 1024 viewBox".into());
    }
    for banned in ["<script", "<foreignobject", "javascript:"] {
        if lower.contains(banned) {
            found.push(format!("contains {banned:?}"));
        }
    }
    // An event handler is an attribute named on…, preceded by whitespace.
    if lower
        .split(|c: char| c.is_whitespace())
        .any(|word| word.starts_with("on") && word.contains('='))
    {
        found.push("an event handler attribute".into());
    }
    // Anything external: an image, a font or a stylesheet fetched by URL.
    for external in ["http://", "https://", "@import", "<image"] {
        let mentions = lower.matches(external).count();
        // The namespace declaration is the one URL every SVG carries.
        let allowed = match external {
            "http://" => lower
                .matches("xmlns=\"http://www.w3.org/2000/svg\"")
                .count(),
            _ => 0,
        };
        if mentions > allowed {
            found.push(format!("references something external ({external})"));
        }
    }
    found
}

#[test]
fn the_logo_is_one_www_would_accept() {
    let found = problems(&logo_text());
    assert!(found.is_empty(), "{LOGO}:\n  {}", found.join("\n  "));
}

// Perturbations: each check goes red on a planted defect.

fn assert_caught(planted: &str, expect: &str) {
    let found = problems(planted);
    assert!(
        found.iter().any(|p| p.contains(expect)),
        "expected a problem containing {expect:?}, got {found:?}"
    );
}

#[test]
fn a_script_is_caught() {
    let planted = logo_text().replace("</svg>", "<script>alert(1)</script></svg>");
    assert_caught(&planted, "<script");
}

#[test]
fn an_event_handler_is_caught() {
    let planted = logo_text().replacen("<rect ", "<rect onclick=\"x()\" ", 1);
    assert_caught(&planted, "event handler");
}

#[test]
fn an_external_image_is_caught() {
    let planted = logo_text().replace(
        "</svg>",
        "<image href=\"https://example.com/a.png\"/></svg>",
    );
    assert_caught(&planted, "external");
}

#[test]
fn a_foreign_object_is_caught() {
    let planted = logo_text().replace("</svg>", "<foreignObject/></svg>");
    assert_caught(&planted, "<foreignobject");
}

#[test]
fn an_oversized_file_is_caught() {
    let planted = format!("{}<!-- {} -->", logo_text(), "x".repeat(MAX_BYTES));
    assert_caught(&planted, "over 2 MB");
}
