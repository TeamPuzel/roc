platform folkertdev/foo
    requires { main : Effect {} }
    exposes []
    packages {}
    imports [ Task ]
    provides [ mainForHost ]
    effects fx.Effect
        {
            putLine : Str -> Effect {},
            putInt : I64 -> Effect {},
            getInt : Effect { value: I64, errorCode: [ A, B ], isError: Bool }
        }

mainForHost : Task.Task {} [] as Fx
mainForHost = main
