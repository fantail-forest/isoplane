//! Native ABI boundaries retain integer widths in unoptimized customer builds.

#[path = "common/no_dns.rs"]
mod no_dns;
#[path = "common/no_egress.rs"]
mod no_egress;

async fn answer(path: &str) -> String {
    let reactor = no_egress::reactor_without_egress(400);
    reactor.context.begin_slice(1, 1);
    let mut guest =
        wisp_harness::ReturnedGuest::new(&wisp_fixtures::compiler_acceptance(), &reactor);
    guest.answer(&reactor, path, 1).await
}

macro_rules! integer_boundary {
    ($name:ident, $path:literal) => {
        #[tokio::test]
        async fn $name() {
            assert_eq!(
                answer($path).await,
                "ok",
                "native integer ABI must preserve the requested signedness and width at {}",
                $path
            );
        }
    };
}

integer_boundary!(unsigned_byte_arguments_are_zero_extended, "/argument/u8");
integer_boundary!(signed_byte_arguments_are_sign_extended, "/argument/i8");
integer_boundary!(unsigned_short_arguments_are_zero_extended, "/argument/u16");
integer_boundary!(signed_short_arguments_are_sign_extended, "/argument/i16");
integer_boundary!(unsigned_byte_returns_are_truncated, "/return/u8");
integer_boundary!(signed_byte_returns_are_sign_extended, "/return/i8");
integer_boundary!(unsigned_short_returns_are_truncated, "/return/u16");
integer_boundary!(signed_short_returns_are_sign_extended, "/return/i16");
integer_boundary!(
    unsigned_byte_callbacks_preserve_the_native_abi,
    "/callback/u8"
);
integer_boundary!(
    signed_byte_callbacks_preserve_the_native_abi,
    "/callback/i8"
);
integer_boundary!(
    unsigned_short_callbacks_preserve_the_native_abi,
    "/callback/u16"
);
integer_boundary!(
    signed_short_callbacks_preserve_the_native_abi,
    "/callback/i16"
);

#[tokio::test]
async fn unoptimized_socket_serialization_preserves_every_port() {
    assert_eq!(
        answer("/serialize").await,
        "ok",
        "IPEndPoint serialization must preserve both port bytes without requiring optimization"
    );
}
