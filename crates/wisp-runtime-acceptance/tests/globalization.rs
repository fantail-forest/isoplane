//! Globalization under the SDK's invariant mode (`InvariantGlobalization`
//! in both profiles: no ICU, one culture) and time zones on a disk that
//! carries no tz database: formatting and parsing are the invariant
//! culture's, comparison is ordinal, named cultures are refused explicitly,
//! named zones come from the tz bundle both packs embed, zones the
//! application builds itself convert, and the guest's wall clock is the
//! host's, in UTC.

#[macro_use]
mod common;
#[path = "common/no_dns.rs"]
mod no_dns;
#[path = "common/no_egress.rs"]
mod no_egress;

use common::{guest, GuestRuntime};
use no_egress::reactor_without_egress;

/// A wall-clock instant on 2026-09-11 (UTC), the date the clock route reports.
const ON_THE_ELEVENTH_MS: u64 = 1_789_100_000_000;

async fn answer(runtime: GuestRuntime, owner: u64, path: &str) -> String {
    let reactor = reactor_without_egress(owner);
    reactor.context.begin_slice(ON_THE_ELEVENTH_MS, 1);
    let mut guest = guest(&reactor, runtime);
    guest.answer(&reactor, path, 1).await
}

in_both_runtimes! {
    /// The one culture is the invariant one; a named culture is refused
    /// (`PredefinedCulturesOnly`, the default under invariant mode) rather
    /// than silently answered with invariant data under another name.
    #[tokio::test]
    async fn the_only_culture_is_the_invariant_one_and_named_cultures_are_refused(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 320, "/globalization/culture").await,
            "current='' ui='' invariant=True known=1 de-DE=CultureNotFoundException en-US=CultureNotFoundException region=IV",
            "invariant mode must expose exactly the invariant culture and refuse de-DE and en-US by name"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn numbers_dates_and_spans_format_with_the_invariant_patterns(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 321, "/globalization/format").await,
            "1,234,567.89|-50.0 %|¤1,234.57|2026-09-11T13:05:07.0000000Z|Friday, 11 September 2026|09/11/2026 13:05|September Fri|01:30:30|0000beef  3.142",
            "the current culture's formatting must be the invariant culture's"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn numbers_and_dates_parse_with_the_invariant_patterns(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 322, "/globalization/parse").await,
            "1234.56|unparsed|2026-09-11T13:05:00|2026-09-11T00:00:00|2026-09-11T11:05:07|42",
            "invariant parsing must accept the invariant separators and month names and refuse a German decimal comma"
        );
    }
}

in_both_runtimes! {
    /// Under invariant mode culture-sensitive comparison is ordinal (with
    /// case folding for the ignore-case options): `a` sorts after `B`,
    /// ligatures and combining sequences are not equivalent, the options
    /// that need collation data are accepted but change nothing, and a sort
    /// key is the big-endian code units.
    #[tokio::test]
    async fn culture_comparison_is_ordinal(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 323, "/globalization/compare").await,
            "a-vs-B=1|a-vs-B-ignore-case=-1|sharp-s=1|ae-ligature=-1|composed=False|ignore-symbols=-1|sorted=Apple,apple,banana,cherry|prefix=True|sort-key=00610062",
            "invariant-mode comparison must be ordinal and sort keys must be the code units"
        );
    }
}

in_both_runtimes! {
    /// Casing is the simple Unicode mapping: no expansion of `ß`, no
    /// Turkish or Greek final forms.
    #[tokio::test]
    async fn casing_is_the_simple_unicode_mapping(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 324, "/globalization/casing").await,
            "STRAßE|8|Ǆ|ΣΣ|Hello Wisp World|Ñ",
            "invariant-mode casing must apply simple one-to-one mappings only"
        );
    }
}

in_both_runtimes! {
    /// Invariant mode has no normalization data: `Normalize` returns its
    /// input and `IsNormalized` is true, while IDN punycode is managed and
    /// works.
    #[tokio::test]
    async fn normalization_is_a_no_op_and_idn_works_without_icu(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 325, "/globalization/normalize").await,
            "composed=False decomposed=1 is-normalized=True idn=xn--bcher-kva.example",
            "under invariant mode normalization must leave the string as it is and IDN must still encode"
        );
    }
}

in_both_runtimes! {
    /// The built-in encodings are present; code-page encodings need the
    /// provider a guest registers itself.
    #[tokio::test]
    async fn built_in_encodings_are_present_and_code_pages_need_a_provider(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 326, "/globalization/encodings").await,
            "registered=7|latin1=28591|cp1252=NotSupportedException|utf8=13|utf16=10|ascii-lossy=h?llo",
            "the seven built-in encodings must be registered and Windows-1252 must be refused without a provider"
        );
    }
}

in_both_runtimes! {
    /// The disk has no tz database; both packs embed the wasm time-zone
    /// bundle, so 313 zones exist and `Europe/London` is found with its
    /// summer offset. Local time is UTC (no `TZ`, no `/etc/localtime`), the
    /// built-in UTC zone is found by id, and the IANA/Windows id maps need
    /// ICU data and answer false.
    #[tokio::test]
    async fn named_time_zones_come_from_the_packs_embedded_data(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 327, "/globalization/timezone").await,
            "utc=UTC local=UTC/00:00:00 local-is-utc=True system-zones=313 london=Europe/London london-offset=01:00:00 utc-by-id=UTC try-london=True iana-to-windows=False/'' windows-to-iana=False/''",
            "local time must be UTC and a named zone must be answered from the pack's embedded tz bundle alone"
        );
    }
}

in_both_runtimes! {
    /// A zone the application constructs, with a daylight rule, converts
    /// both ways without data, as does the built-in UTC zone by id.
    #[tokio::test]
    async fn a_custom_time_zone_with_a_daylight_rule_converts_both_ways(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 328, "/globalization/timezone-custom").await,
            "2026-09-11T14:00:00/True|2026-12-11T13:00:00/False|2026-09-11T12:00:00/Utc|2026-09-11T12:00:00|2026-09-11T12:00:00|2026-09-11T14:00:00+02:00|02:00:00",
            "conversions through an application-built zone must honour its base offset and daylight rule"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn the_wall_clock_is_the_hosts_and_local_time_is_utc(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 329, "/globalization/clock").await,
            "2026-09-11|Utc|Local|00:00:00|00:00:00|00:00:00",
            "DateTime.UtcNow must be the slice's clock and DateTime.Now must equal it with a zero offset"
        );
    }
}
