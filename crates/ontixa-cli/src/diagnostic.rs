//! Offline, authored diagnostic guides. Examples are bundled at build time;
//! looking up a code never loads a workspace or runs the compiler.

use crate::envelope::Envelope;
use ontixa_diagnostics::{Code, Diagnostics};
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeSet;
use std::process::ExitCode;

#[derive(Serialize)]
struct Guide {
    code: Code,
    title: &'static str,
    meaning: &'static str,
    cause: &'static str,
    correction: &'static str,
    failing_example: &'static str,
    corrected_example: &'static str,
    see_also: &'static [&'static str],
}

const GUIDES: &[Guide] = &[
    Guide {
        code: Code::UseAfterMove,
        title: "Use after ownership transfer",
        meaning: "The binding no longer owns a usable value, or may have lost it on a previous path.",
        cause: "Moving a non-Copy value into another binding or passing it to a move, escape, or unknown parameter consumes it. In this example, keep returns its argument, so its inferred contract is escape: ownership transfers from p to q. Data values and arrays are not Copy.",
        correction: "Use the returned owner q instead of the moved binding p. If a callee only needs to read a value, write its body to read without consuming it; Ontixa infers borrow from that behavior. Inspect a callee with ontixa explain file.ixa keep. Adding mut alone does not restore a moved value, and Ontixa has no source-level borrow annotations.",
        failing_example: include_str!("../../../examples/diagnostics/use-after-move.ixa"),
        corrected_example: include_str!("../../../examples/diagnostics/use-after-move-fixed.ixa"),
        see_also: &[
            "docs/memory-model.md#how-inference-works",
            "docs/contracts.md#ownership-contracts-milestone-1",
            "examples/borrow-inference.ixa",
        ],
    },
    Guide {
        code: Code::Uninitialized,
        title: "Read before initialization",
        meaning: "The binding has no value, or is not definitely initialized on every path reaching the read.",
        cause: "A type annotation declares what a binding can hold; it does not supply a value. An assignment in only one if branch, or only in the right-hand side of && or ||, may be skipped and does not establish definite initialization.",
        correction: "Assign a value before reading it, as shown here, or supply an initializer in the let declaration. For conditional initialization, assign on every continuing path. An immutable binding permits one deferred initialization, so this example does not need mut.",
        failing_example: include_str!("../../../examples/diagnostics/uninitialized.ixa"),
        corrected_example: include_str!("../../../examples/diagnostics/uninitialized-fixed.ixa"),
        see_also: &[
            "docs/memory-model.md#how-inference-works",
            "docs/memory-model.md#mutability-immutable-by-default",
        ],
    },
    Guide {
        code: Code::ImmutableAssignment,
        title: "Assignment without mutable authority",
        meaning: "An assignment or field write targets a binding that was not declared mut.",
        cause: "Bindings are immutable by default. After count receives its initial value, assigning to it again needs explicit mutable authority. Field writes also require mut on the root binding.",
        correction: "If reassignment is intended, declare let mut count as in the corrected example. A parameter that must be mutated uses mut before its name. If the value should stay immutable, compute a new binding instead. One deferred first initialization is allowed without mut; reassignment or reinitialization after a move requires mut.",
        failing_example: include_str!("../../../examples/diagnostics/immutable-assignment.ixa"),
        corrected_example: include_str!(
            "../../../examples/diagnostics/immutable-assignment-fixed.ixa"
        ),
        see_also: &[
            "docs/memory-model.md#mutability-immutable-by-default",
            "docs/adr/0009-binding-mutability.md",
        ],
    },
    Guide {
        code: Code::MutableBorrowOfImmutable,
        title: "Mutating call with an immutable argument",
        meaning: "A callee with an inferred borrow_mut parameter received a place whose root binding is immutable.",
        cause: "The body of bump writes p.x, so the compiler infers borrow_mut. Its mut parameter permits that write inside bump, but does not give the caller's immutable p permission to change.",
        correction: "If the call should mutate the caller's value, declare that binding with let mut, as shown here. The call borrows it mutably and the caller can read it afterwards. Inspect the inferred contract with ontixa explain file.ixa bump. A fresh temporary needs no mutable binding authority, but changing an existing value requires it.",
        failing_example: include_str!(
            "../../../examples/diagnostics/mutable-borrow-of-immutable.ixa"
        ),
        corrected_example: include_str!(
            "../../../examples/diagnostics/mutable-borrow-of-immutable-fixed.ixa"
        ),
        see_also: &[
            "docs/memory-model.md#mutability-immutable-by-default",
            "docs/adr/0009-binding-mutability.md",
            "examples/borrow-inference.ixa",
        ],
    },
];

fn guide_for(code: Code) -> Option<&'static Guide> {
    GUIDES.iter().find(|guide| guide.code == code)
}

/// Prints at most one lookup hint per covered code, only from human `check`.
/// The diagnostic values and their JSON rendering remain unchanged.
pub fn print_check_hints(diags: &Diagnostics) {
    let codes: BTreeSet<Code> = diags.iter().map(|d| d.code).collect();
    for code in codes {
        if guide_for(code).is_some() {
            eprintln!("help: for an explanation and examples, run `ontixa diagnostic {code}`");
        }
    }
}

/// Handles the source-independent `diagnostic <CODE>` command.
pub fn run(code: &str, json_output: bool) -> ExitCode {
    let known = Code::from_code(code);
    if let Some(guide) = known.and_then(guide_for) {
        if json_output {
            return Envelope::new("diagnostic")
                .result(json!({"guide": guide}))
                .emit();
        }
        println!("{}: {}\n", guide.code, guide.title);
        println!("Meaning\n{}\n", guide.meaning);
        println!("Common cause\n{}\n", guide.cause);
        println!("Failing example\n{}", guide.failing_example);
        println!("Correction\n{}\n", guide.correction);
        println!("Corrected example\n{}", guide.corrected_example);
        println!("Read more (repository-relative paths)");
        for path in guide.see_also {
            println!("  {path}");
        }
        return ExitCode::SUCCESS;
    }

    let (kind, message) = if known.is_some() {
        (
            "diagnostic_guide_unavailable",
            format!("`{code}` is a recognized diagnostic code, but has no offline guide yet"),
        )
    } else {
        (
            "unknown_diagnostic_code",
            format!("unknown diagnostic code `{code}`; codes are case-sensitive"),
        )
    };
    let available: Vec<&str> = GUIDES.iter().map(|guide| guide.code.as_str()).collect();
    if json_output {
        Envelope::new("diagnostic")
            .result(json!({"code": code, "available_codes": available}))
            .error(kind, message, 2)
            .emit()
    } else {
        eprintln!("error: {message}");
        eprintln!("Available guides: {}", available.join(", "));
        eprintln!("See docs/diagnostics.md for the full code catalog.");
        ExitCode::from(2)
    }
}
