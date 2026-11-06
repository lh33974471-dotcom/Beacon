// make_args! and dispatch_script_call! are derived from sciter-rs (MIT, (c) 2019
// pravic); see THIRD-PARTY-NOTICES.md. Consumer code depends on their exact expansion.

#[macro_export]
macro_rules! make_args {
    () => { { let args : [$crate::value::Value; 0] = []; args } };

    ( $($s:expr),* ) => {
        {
            let args = [
            $(
                $crate::value::Value::from($s)
             ),*
            ];
            args
        }
    };
}

#[macro_export]
macro_rules! dispatch_script_call {
    (
        $(
            fn $name:ident ( $( $argt:ident ),* );
         )*
    ) => {
        fn dispatch_script_call(&mut self, _root: $crate::HELEMENT, name: &str, argv: &[$crate::Value]) -> Option<$crate::Value>
        {
            match name {
                $(
                    stringify!($name) => {
                        let mut _i = 0;
                        $(
                            let _: Option<$argt> = None;
                            _i += 1;
                        )*
                        let argc = _i;

                        if argv.len() != argc {
                            return Some($crate::Value::error(&format!("{} error: {} of {} arguments provided.", stringify!($name), argv.len(), argc)));
                        }

                        let mut _i = 0;
                        let rv = self.$name(
                            $(
                                {
                                    match $crate::FromValue::from_value(&argv[_i]) {
                                        Some(arg) => { _i += 1; let arg: $argt = arg; arg },
                                        None => {
                                            return Some($crate::Value::error(&format!("{} error: invalid type of {} argument ({} expected, {:?} provided).",
                                                stringify!($name), _i, stringify!($argt), argv[_i])));
                                        },
                                    }
                                }
                             ),*
                        );

                        return Some($crate::Value::from(rv));
                    },
                 )*

                _ => ()
            };

            return None;
        }
    };
}
