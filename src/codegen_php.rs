//! PHP backend — core-subset native seat over [`crate::codegen_core`].
//!
//! Emits real PHP: `cuni_say` helper (bool → `True`/`False`), `$`-prefixed
//! variables, `intdiv` for truncating int division, `cuni_mod` for
//! Python-floored `%` (PHP's `%` truncates like C), `.` for string concat.
//! Refuses everything beyond the core subset (see codegen_core docs).

use crate::ast::Program;
use crate::codegen_core::{generate as core_generate, LangSpec};

fn php_str(s: &str) -> String {
    let mut r = String::from('"');
    for c in s.chars() {
        match c {
            '"' => r.push_str("\\\""),
            '\\' => r.push_str("\\\\"),
            '\n' => r.push_str("\\n"),
            '\t' => r.push_str("\\t"),
            '\r' => r.push_str("\\r"),
            '$' => r.push_str("\\$"),
            c if (c as u32) < 0x20 => r.push_str(&format!("\\u{{{}}}", c as u32)),
            c => r.push(c),
        }
    }
    r.push('"');
    r
}

const SPEC: LangSpec = LangSpec {
    comment: "<?php //",
    header: "function cuni_say($x, $ty) {\n    if ($ty === \"bool\") { echo $x ? \"True\\n\" : \"False\\n\"; }\n    else { echo $x . \"\\n\"; }\n}\nfunction cuni_mod($a, $b) { return (($a % $b) + $b) % $b; }\nfunction cuni_dec_mul($a, $b) { if ($b != 0 && abs($a) > intdiv(PHP_INT_MAX, abs($b))) { throw new Exception(\"cuni: dec multiplication overflow\"); } return intdiv($a * $b, 10000); }\nfunction cuni_dec_div($a, $b) { if ($b == 0) { throw new Exception(\"cuni: dec division by zero\"); } if (abs($a) > intdiv(PHP_INT_MAX, 10000)) { throw new Exception(\"cuni: dec division intermediate overflow\"); } return intdiv($a * 10000, $b); }\nfunction cuni_dec_str($v) { $s = \"\"; if ($v < 0) { $s = \"-\"; $v = -$v; } $ip = intdiv($v, 10000); $fr = $v % 10000; $fs = sprintf(\"%04d\", $fr); $fs = rtrim($fs, \"0\"); if ($fs === \"\") { $fs = \"0\"; } return $s . $ip . \".\" . $fs; }\n",
    main_open: "",
    main_close: "",
    let_fmt: "{name} = {expr};",
    mut_fmt: "{name} = {expr};",
    assign_fmt: "{name} = {expr};",
    fn_open_fmt: "function {name}({params}) {",
    fn_close: "}",
    ret_fmt: "return {expr};",
    if_open_fmt: "if ({cond}) {",
    else_open: "} else {",
    block_close: "}",
    while_open_fmt: "while ({cond}) {",
    say_fmt: "cuni_say({expr}, '{ty}');",
    call_fmt: "{name}({args})",
    true_lit: "true",
    false_lit: "false",
    str_quote: php_str,
    // PHP `/` on ints yields float; intdiv() truncates toward zero (CuNi).
    int_div_helper: Some("intdiv"),
    // PHP `%` truncates like C; CuNi wants Python-floored.
    int_mod_helper: Some("cuni_mod"),
    // dec (docs/DECIMAL.md §3, §6): intdiv() truncates; mul/div helpers
    // pre-check the int64 envelope and refuse loudly on overflow.
    dec_mul_helper: Some("cuni_dec_mul"),
    dec_div_helper: Some("cuni_dec_div"),
    dec_str_helper: Some("cuni_dec_str"),
    dec_lit_suffix: "",
    dec_max_abs: Some(9_223_372_036_854_775_807),
    str_eq_op: "==",
    str_ne_op: "!=",
    str_lt_op: "<",
    str_gt_op: ">",
    str_le_op: "<=",
    str_ge_op: ">=",
    concat_fmt: "({l} . {r})",
    var_prefix: "$",
    indent: "    ",
};

pub fn generate(program: &Program) -> Result<String, String> {
    core_generate(program, &SPEC)
}
