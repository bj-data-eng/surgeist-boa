use boa_macros::js_str;
use indoc::indoc;

use crate::{JsNativeErrorKind, TestAction, run_test_actions};

#[test]
// https://github.com/boa-dev/boa/issues/2317
fn fun_block_eval_2317() {
    run_test_actions([
        TestAction::assert_eq(
            indoc! {r#"
                (function(y){
                    {
                        eval("var x = 'inner';");
                    }
                    return y + x;
                })("arg");
            "#},
            js_str!("arginner"),
        ),
        TestAction::assert_eq(
            indoc! {r#"
                (function(y = "default"){
                    {
                        eval("var x = 'inner';");
                    }
                    return y + x;
                })();
            "#},
            js_str!("defaultinner"),
        ),
    ]);
}

#[test]
// https://github.com/boa-dev/boa/issues/2719
fn with_env_not_panic() {
    run_test_actions([TestAction::assert_native_error(
        indoc! {r#"
            with({ p1:1,  }) {k[oa>>2]=d;}
            {
            let a12345678901234567890123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890 = 1,
                b = "";
            }
        "#},
        JsNativeErrorKind::Reference,
        "k is not defined",
    )]);
}

#[test]
// https://github.com/boa-dev/boa/issues/4350
fn indirect_eval_function_var_binding_4350() {
    run_test_actions([TestAction::assert_eq(
        indoc! {r#"
            var t = [];

            var s1 = `
            function core() { t.push(1) }

            core.prototype.a = function () { t.push(2) }
            core.prototype.b = function () { t.push(3) }
            `;
            var s2 = `
            function core() { t.push(1) }

            core.prototype.a = function () { t.push(2) }
            core.prototype.b = function () { t.push(3) }
            var core = new core();
            `;
            var s3 = `
            function core() { t.push(1) }
            var core = new core();
            `;

            function run_ctx(s) {
                (1,eval)(s);
            }

            function test() {
                run_ctx(s1);
                var core1 = new core();

                run_ctx(s2);
                var core2 = core;

                run_ctx(s3);
                var core3 = core;
                return [core1, core2, core3].toString();
            }

            test();
        "#},
        js_str!("[object Object],[object Object],[object Object]"),
    )]);
}

#[test]
// https://github.com/boa-dev/boa/issues/5333
fn eval_created_bindings_can_be_deleted_5333() {
    run_test_actions([
        TestAction::assert_eq(
            indoc! {r#"
                (function() {
                    var initial = null;
                    var deleted = null;
                    var postDeletion;
                    eval('initial = x; deleted = delete x; postDeletion = function() { x; }; var x;');
                    try {
                        postDeletion();
                        return 'no throw';
                    } catch (e) {
                        return String(initial) + ':' + String(deleted) + ':' + e.name;
                    }
                }());
            "#},
            js_str!("undefined:true:ReferenceError"),
        ),
        TestAction::assert_eq(
            indoc! {r#"
                (function() {
                    var initial;
                    var deleted = null;
                    var postDeletion;
                    eval('initial = f; deleted = delete f; postDeletion = function() { f; }; function f() { return 33; }');
                    try {
                        postDeletion();
                        return 'no throw';
                    } catch (e) {
                        return typeof initial + ':' + String(initial()) + ':' + String(deleted) + ':' + e.name;
                    }
                }());
            "#},
            js_str!("function:33:true:ReferenceError"),
        ),
        TestAction::assert_eq(
            indoc! {r#"
                (function() {
                    delete globalThis.x;
                    eval('delete x; var x = 1;');
                    var result = typeof globalThis.x + ':' + String(globalThis.x);
                    delete globalThis.x;
                    return result;
                }());
            "#},
            js_str!("number:1"),
        ),
        TestAction::assert_eq(
            indoc! {r#"
                (function() {
                    delete globalThis.x;
                    var result = eval('var x = delete x; x;');
                    var global = globalThis.x;
                    delete globalThis.x;
                    return String(result) + ':' + String(global);
                }());
            "#},
            js_str!("true:true"),
        ),
        TestAction::assert_eq(
            indoc! {r#"
                (function() {
                    delete globalThis.x;
                    var x = 'outer';
                    var result = (function() {
                        return eval('var x = delete x; x;');
                    }());
                    var global = globalThis.x;
                    delete globalThis.x;
                    return String(result) + ':' + String(x) + ':' + String(global);
                }());
            "#},
            js_str!("true:true:undefined"),
        ),
        TestAction::assert_eq(
            indoc! {r#"
                (function() {
                    delete globalThis.x;
                    eval('var x; delete x;');
                    eval('var x = 2;');
                    var result = String(x) + ':' + String(globalThis.x);
                    delete globalThis.x;
                    return result;
                }());
            "#},
            js_str!("2:undefined"),
        ),
        TestAction::assert_eq(
            indoc! {r#"
                (function() {
                    delete globalThis.f;
                    eval('function f() {}; delete f;');
                    eval('function f() { return 2; }');
                    var result = String(f()) + ':' + String(globalThis.f);
                    delete globalThis.f;
                    return result;
                }());
            "#},
            js_str!("2:undefined"),
        ),
    ]);
}

#[test]
// https://github.com/boa-dev/boa/issues/5332
fn direct_eval_bindings_are_local_to_each_call() {
    run_test_actions([
        TestAction::run("function run(source) { return eval(source); }"),
        TestAction::assert_eq(
            r#"run("eval('var evalLocal = 1;'); typeof evalLocal")"#,
            js_str!("number"),
        ),
        TestAction::assert_eq(r#"run("typeof evalLocal")"#, js_str!("undefined")),
    ]);
}

#[test]
// https://github.com/boa-dev/boa/issues/5332
fn direct_eval_bindings_are_local_to_each_function_instance() {
    run_test_actions([TestAction::assert_eq(
        indoc! {r#"
            function makeRun(label) {
                return function(source) {
                    return label + ":" + eval(source);
                };
            }
            let first = makeRun("first");
            let second = makeRun("second");
            [
                first("eval('var evalLocal = 1;'); typeof evalLocal"),
                second("typeof evalLocal"),
            ].join(",");
        "#},
        js_str!("first:number,second:undefined"),
    )]);
}

#[test]
fn direct_eval_bindings_remain_live_in_closures() {
    run_test_actions([TestAction::assert_eq(
        indoc! {r#"
            function capture(label) {
                let count = 0;
                const next = () => label + ":" + ++count + ":" + evalLocal;
                eval("var evalLocal = label;");
                eval("evalLocal += '!';");
                return next;
            }
            let first = capture("first");
            let second = capture("second");
            [first(), first(), second(), first()].join(",");
        "#},
        js_str!("first:1:first!,first:2:first!,second:1:second!,first:3:first!"),
    )]);
}

#[test]
fn direct_eval_bindings_can_shadow_an_outer_constant() {
    run_test_actions([TestAction::assert_eq(
        indoc! {r#"
            const evalLocal = "outer";
            function run(declare) {
                const read = () => eval("evalLocal");
                const increment = () => eval("evalLocal += 1; evalLocal");
                if (!declare) {
                    return read();
                }
                eval("var evalLocal = 1;");
                const current = eval("evalLocal += 1; evalLocal");
                return [current, increment()].join(",");
            }
            [run(true), run(false), evalLocal].join(",");
        "#},
        js_str!("2,3,outer,outer"),
    )]);
}
