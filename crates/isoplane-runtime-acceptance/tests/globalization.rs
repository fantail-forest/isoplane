//! ICU-backed culture, Unicode, calendar and time-zone behavior in Mono.

#[macro_use]
mod common;

use common::{guest, GuestRuntime};
use isoplane_engine::Completion;

const ON_THE_ELEVENTH_MS: u64 = 1_789_100_000_000;

async fn answer(runtime: GuestRuntime, owner: u64, path: &str) -> String {
    let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId(owner));
    reactor.context.begin_slice(ON_THE_ELEVENTH_MS, 1);
    let mut guest = guest(&reactor, runtime);
    guest.answer(&reactor, path, 1).await
}

in_mono_runtime! {
    #[tokio::test]
    async fn named_cultures_are_available_while_the_default_culture_is_invariant(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 320, "/globalization/culture").await,
            "current='' ui='' invariant=True named-cultures=True de-DE='de-DE' en-US='en-US' region=IV",
            "the empty environment must default to invariant culture while exposing ICU named cultures"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn numbers_dates_and_spans_format_with_the_invariant_patterns(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 321, "/globalization/format").await,
            "1,234,567.89|-50.0 %|¤1,234.57|2026-09-11T13:05:07.0000000Z|Friday, 11 September 2026|09/11/2026 13:05|September Fri|01:30:30|0000beef  3.142",
            "the current culture's formatting must be the invariant culture's"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn numbers_and_dates_parse_with_the_invariant_patterns(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 322, "/globalization/parse").await,
            "1234.56|unparsed|2026-09-11T13:05:00|2026-09-11T00:00:00|2026-09-11T11:05:07|42",
            "invariant parsing must accept the invariant separators and month names and refuse a German decimal comma"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn culture_comparison_uses_unicode_collation_data(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 323, "/globalization/compare").await,
            "composed=True ignore-symbols=True swedish-z-before-a-ring=True japanese-width=True",
            "culture comparisons must honor canonical equivalence, language sorting, symbols and width"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn casing_honors_turkish_and_greek_cultures(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 324, "/globalization/casing").await,
            "İSTANBUL/ı/ΑΒΓ",
            "named cultures must provide their language-specific casing"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn unicode_normalization_and_idn_use_standard_data(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 325, "/globalization/normalize").await,
            "composed=True decomposed=2 is-normalized=True idn=xn--bcher-kva.example",
            "normalization must compose and decompose equivalent Unicode strings and IDN must encode"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn built_in_encodings_are_present_and_code_pages_need_a_provider(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 326, "/globalization/encodings").await,
            "registered=7|latin1=28591|cp1252=NotSupportedException|utf8=13|utf16=10|ascii-lossy=h?llo",
            "the seven built-in encodings must be registered and Windows-1252 must be refused without a provider"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn named_time_zones_come_from_the_packs_embedded_data(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 327, "/globalization/timezone").await,
            "utc=UTC local=UTC/00:00:00 local-is-utc=True new-york-winter=-5 london=Europe/London london-offset=01:00:00 utc-by-id=UTC try-london=True iana-to-windows=True/'GMT Standard Time' windows-to-iana=True/'Europe/London'",
            "local time must be UTC and a named zone must be answered from the pack's embedded tz bundle alone"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn a_custom_time_zone_with_a_daylight_rule_converts_both_ways(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 328, "/globalization/timezone-custom").await,
            "2026-09-11T14:00:00/True|2026-12-11T13:00:00/False|2026-09-11T12:00:00/Utc|2026-09-11T12:00:00|2026-09-11T12:00:00|2026-09-11T14:00:00+02:00|02:00:00",
            "conversions through an application-built zone must honour its base offset and daylight rule"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn the_wall_clock_is_the_hosts_and_local_time_is_utc(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 329, "/globalization/clock").await,
            "2026-09-11|Utc|Local|00:00:00|00:00:00|00:00:00",
            "DateTime.UtcNow must be the slice's clock and DateTime.Now must equal it with a zero offset"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn named_cultures_format_and_parse_their_decimal_and_group_separators(runtime: GuestRuntime) {
        assert_eq!(answer(runtime, 330, "/globalization/named-format").await, "1.234,5/1234.5",
            "German number formatting and parsing must use ICU culture data");
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn non_gregorian_calendars_are_available(runtime: GuestRuntime) {
        assert_eq!(answer(runtime, 331, "/globalization/calendars").await, "hebrew=5784 japanese=1",
            "Hebrew year and Japanese era transitions must match their calendar data");
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn non_latin_and_compatibility_normalization_use_unicode_data(runtime: GuestRuntime) {
        assert_eq!(answer(runtime, 332, "/globalization/unicode-forms").await,
            "hangul=True decomposed=True compatibility=True compatibility-decomposed=True needs-normalization=True",
            "Hangul composition and decomposition and compatibility forms must use Unicode normalization data");
    }
}

in_mono_runtime! {
    #[tokio::test(start_paused = true)]
    async fn a_named_culture_remains_usable_after_a_hibernation_delta(runtime: GuestRuntime) {
        let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId(333));
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start("/globalization/culture-after-park");
        assert_eq!(outcome.completion, Completion::Parked, "the culture-bearing continuation must park: {outcome:?}");
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_millis(60)).await;
        reactor.context.begin_slice(61, 1);
        let mut outcome = guest.wake_through_delta(&reactor).await;
        for _ in 0..8 {
            if outcome.completion != Completion::Parked {
                break;
            }
            outcome = guest.wake(&reactor).await;
        }
        assert_eq!(outcome.completion, Completion::Done, "ICU must remain usable after restoring the guest: {outcome:?}");
        assert_eq!(reactor.response_text(1), "tr-TR/İSTANBUL", "the current culture and its ICU-backed casing must survive hibernation");
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn a_locale_outside_the_browser_subset_has_its_own_day_names(runtime: GuestRuntime) {
        assert_eq!(answer(runtime, 334, "/globalization/welsh-day").await,
            "Dydd Gwener", "Welsh date formatting must use its locale data rather than a fallback culture");
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn non_gregorian_calendar_month_names_use_their_cultures_data(runtime: GuestRuntime) {
        assert_eq!(answer(runtime, 335, "/globalization/hebrew-month").await,
            "טבת", "the Hebrew calendar must supply its own month names");
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn windows_time_zone_ids_resolve_and_observe_daylight_rules(runtime: GuestRuntime) {
        assert_eq!(answer(runtime, 336, "/globalization/windows-zone").await,
            "-05:00:00/-04:00:00", "Windows time zone identifiers must resolve through ICU to the embedded time zone rules");
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn an_unknown_normalization_form_is_rejected(runtime: GuestRuntime) {
        assert_eq!(answer(runtime, 337, "/globalization/invalid-normalization-form").await,
            "ArgumentException", "enabling compatibility normalization must preserve invalid-form rejection");
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn normalization_rejects_an_unpaired_surrogate(runtime: GuestRuntime) {
        assert_eq!(answer(runtime, 338, "/globalization/invalid-normalization-input").await,
            "ArgumentException", "compatibility normalization must reject malformed UTF-16 rather than emit corrupted text");
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn system_timezones_have_localized_standard_and_daylight_names(runtime: GuestRuntime) {
        assert_eq!(answer(runtime, 339, "/globalization/timezone-names").await,
            "Eastern Standard Time|Eastern Daylight Time",
            "system timezone names must use full ICU display-name data instead of abbreviations");
    }
}
