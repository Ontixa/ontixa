use ontixa_interpreter::{Interp, Value};

fn remainder(ty: &str, lhs: &str, rhs: &str) -> f64 {
    let src = format!(
        "fn rem(a: {ty}, b: {ty}) -> {ty} {{ a % b }}
         fn main() -> {ty} {{ rem({lhs}, {rhs}) }}"
    );
    let (mir, module, _, _, interner, diags) = ontixa_mir::mir_src(&src);
    assert!(diags.is_empty(), "{src}: {diags:?}");
    match Interp::new(&mir, &module, &interner).run("main") {
        Ok(Value::Float(value)) => value,
        other => panic!("{src}: expected float, got {other:?}"),
    }
}

#[test]
fn finite_remainder_uses_truncating_division() {
    for ty in ["f32", "f64"] {
        for (lhs, rhs, expected) in [
            ("5.5", "2.0", 1.5),
            ("-5.5", "2.0", -1.5),
            ("5.5", "-2.0", 1.5),
            ("-5.5", "-2.0", -1.5),
            ("1.5", "2.0", 1.5),
            ("-1.5", "2.0", -1.5),
            ("50.5", "8.125", 1.75),
        ] {
            assert_eq!(remainder(ty, lhs, rhs), expected, "{ty}: {lhs} % {rhs}");
        }
    }
}

#[test]
fn zero_remainder_keeps_the_dividend_sign() {
    for ty in ["f32", "f64"] {
        for rhs in ["2.0", "-2.0", "1.0 / 0.0", "-1.0 / 0.0"] {
            for (lhs, expected) in [("0.0", 0.0_f64), ("-0.0", -0.0_f64)] {
                assert_eq!(remainder(ty, lhs, rhs).to_bits(), expected.to_bits());
            }
        }
        for rhs in ["2.0", "-2.0"] {
            for (lhs, expected) in [("4.0", 0.0_f64), ("-4.0", -0.0_f64)] {
                assert_eq!(remainder(ty, lhs, rhs).to_bits(), expected.to_bits());
            }
        }
    }
}

#[test]
fn finite_remainder_by_infinity_returns_the_dividend() {
    for ty in ["f32", "f64"] {
        for rhs in ["1.0 / 0.0", "-1.0 / 0.0"] {
            for (lhs, expected) in [("5.5", 5.5), ("-5.5", -5.5)] {
                assert_eq!(remainder(ty, lhs, rhs), expected);
            }
        }
    }
}

#[test]
fn zero_divisor_infinite_dividend_and_nan_produce_nan() {
    for ty in ["f32", "f64"] {
        for (lhs, rhs) in [
            ("5.5", "0.0"),
            ("-5.5", "-0.0"),
            ("0.0", "0.0"),
            ("1.0 / 0.0", "2.0"),
            ("-1.0 / 0.0", "-2.0"),
            ("1.0 / 0.0", "1.0 / 0.0"),
            ("0.0 / 0.0", "2.0"),
            ("5.5", "0.0 / 0.0"),
            ("0.0 / 0.0", "0.0 / 0.0"),
        ] {
            assert!(remainder(ty, lhs, rhs).is_nan(), "{ty}: {lhs} % {rhs}");
        }
    }
}
