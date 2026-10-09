//! Human annotations retain every span, including spans sharing a source line.

use ontixa_diagnostics::{Code, Diagnostic, render};
use ontixa_source::{FileId, SourceFile, Span};

fn file(text: &str) -> SourceFile {
    SourceFile::new(FileId::new(0), "test.ixa", None, text.to_string())
}

#[test]
fn earlier_label_does_not_hide_unlabeled_primary() {
    let d = Diagnostic::error(Code::UseAfterMove, "moved")
        .primary(Span::new(4, 5))
        .label(Span::new(0, 1), "moved here");
    assert_eq!(
        render(&d, &file("p q p\n")),
        "error[E_USE_AFTER_MOVE]: moved\n  --> test.ixa:1:5\n  |\n1 | p q p\n  | - moved here\n  |     ^ \n  |\n"
    );
}

#[test]
fn primary_does_not_hide_later_labels_and_labels_follow_source_order() {
    let d = Diagnostic::error(Code::UseAfterMove, "moved")
        .primary(Span::new(0, 1))
        .label(Span::new(4, 5), "last")
        .label(Span::new(2, 3), "middle");
    assert_eq!(
        render(&d, &file("p q r\n")),
        "error[E_USE_AFTER_MOVE]: moved\n  --> test.ixa:1:1\n  |\n1 | p q r\n  | ^ \n  |   - middle\n  |     - last\n  |\n"
    );
}

#[test]
fn overlapping_and_coincident_annotations_keep_every_message() {
    let d = Diagnostic::error(Code::UseAfterMove, "moved")
        .primary(Span::new(1, 4))
        .label(Span::new(2, 5), "overlapping")
        .label(Span::new(1, 4), "coincident first")
        .label(Span::new(0, 3), "earlier")
        .label(Span::new(1, 4), "coincident second");
    let text = render(&d, &file("abcdef\n"));
    assert_eq!(text.matches("1 | abcdef").count(), 1);
    let annotations: Vec<_> = text
        .lines()
        .filter(|line| line.contains('^') || line.contains("- "))
        .collect();
    assert_eq!(annotations.len(), 5, "{text}");
    assert!(annotations[0].ends_with("earlier"));
    assert!(annotations[1].contains('^'));
    assert!(annotations[2].ends_with("coincident first"));
    assert!(annotations[3].ends_with("coincident second"));
    assert!(annotations[4].ends_with("overlapping"));
}

#[test]
fn unicode_and_tab_prefixes_align_each_annotation() {
    let source = "\té p q\n";
    let p = source.find('p').unwrap() as u32;
    let q = source.find('q').unwrap() as u32;
    let d = Diagnostic::error(Code::UseAfterMove, "moved")
        .primary(Span::new(q, q + 1))
        .label(Span::new(p, p + 1), "moved here");
    assert_eq!(
        render(&d, &file(source)),
        "error[E_USE_AFTER_MOVE]: moved\n  --> test.ixa:1:6\n  |\n1 | \té p q\n  | \t  - moved here\n  | \t    ^ \n  |\n"
    );
}

#[test]
fn separate_lines_keep_the_existing_context_and_gap_layout() {
    let d = Diagnostic::error(Code::UseAfterMove, "moved")
        .primary(Span::new(6, 7))
        .label(Span::new(0, 1), "moved here")
        .note("a note")
        .help("a suggestion");
    assert_eq!(
        render(&d, &file("p\nx\nx\np\n")),
        "error[E_USE_AFTER_MOVE]: moved\n  --> test.ixa:4:1\n  |\n1 | p\n  | - moved here\n  ...\n4 | p\n  | ^ \n  |\n  = note: a note\n  = help: a suggestion\n"
    );
}

#[test]
fn lone_empty_primary_and_spanless_diagnostic_keep_existing_layout() {
    let d = Diagnostic::error(Code::Parse, "expected expression").primary(Span::empty(1));
    assert_eq!(
        render(&d, &file("p")),
        "error[E_PARSE]: expected expression\n  --> test.ixa:1:2\n  |\n1 | p\n  |  ^ \n  |\n"
    );
    assert_eq!(
        render(&Diagnostic::error(Code::Parse, "no location"), &file("")),
        "error[E_PARSE]: no location\n  --> test.ixa:1:1\n  |\n"
    );
}
