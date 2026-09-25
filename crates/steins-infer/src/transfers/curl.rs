use steins_domain::{Base, Fact};
use steins_syntax::{ArgValue, RefKind};

/// `curl_getinfo($handle, CURLINFO_X)` → **the fixed type php.net documents for
/// `CURLINFO_X`**, on the `$option` argument being a recognized `CURLINFO_*`
/// global constant NAME (issue #594). The constant's VALUE is never read: by
/// design (issue #168) an `ArgValue::GlobalConst` carries no proven value, and
/// this table has no use for one anyway — the name alone is the whole key.
///
/// # Method
///
/// Every row is the `gettype()` PHP itself reports for
/// `curl_getinfo($h, CURLINFO_X)` on a `curl_init($url)` handle that is NEVER
/// `curl_exec`'d — no network is reached (ADR-0061 §4) — cross-checked against
/// php.net's `curl_getinfo` return-value table. Witnessed at `PINNED_PHP`
/// (8.5.9, linked libcurl 8.7.1).
///
/// This works because an **int- or float-typed** option coerces libcurl's
/// unset-field sentinel (`0`, or `-1` for the handful documented that way) at
/// the C level rather than ever answering PHP `false` — so a virgin handle
/// already carries the same `integer`/`double` `gettype()` the table below
/// claims for a live one. A **string-typed** option is not uniform: most
/// coalesce an unset C `NULL` to PHP `false` (the confirmed `T|false` rows,
/// below), but four coalesce it to `''` instead — verified because the probe
/// answers a real (non-`false`) string on the same untouched handle. Those four
/// are the `STRING` list's entire membership.
///
/// # The declines, each measured or reasoned from a primary source
///
/// * **Every `string|false`/`mixed`/array-shaped documented option**
///   (`CURLINFO_CONTENT_TYPE`, `CURLINFO_REDIRECT_URL`, `CURLINFO_HEADER_OUT`,
///   `CURLINFO_PRIVATE`, `CURLINFO_FTP_ENTRY_PATH`, `CURLINFO_REFERER`,
///   `CURLINFO_CAINFO`, `CURLINFO_CAPATH`, `CURLINFO_RTSP_SESSION_ID`, …) — no
///   `Fact` spells a two-base union, the same floor `min`/`json_decode` already
///   stand on. `CURLINFO_PRIVATE` specifically echoes back whatever
///   `CURLOPT_PRIVATE` was set to — `mixed` by construction, not by measurement.
/// * **`CURLINFO_SCHEME`** — php.net's own return-value table calls this
///   `string`, but the probe answers `bool(false)` on the untouched handle
///   exactly like the confirmed `T|false` rows above, unlike `STRING`'s four
///   members (which default to `''`). Trusting the documented word against the
///   grain of its own measurement is exactly the transcription ADR-0061 §4
///   forbids, so this stays unrecognized pending a stronger signal.
/// * **`CURLINFO_POSTTRANSFER_TIME_T`** — PHP 8.4.0 gates the *constant*, but
///   php.net's changelog additionally gates the *value* on cURL ≥ 8.10.0 (this
///   probe's libcurl 8.7.1 is older, yet still answers a plain `int` — an
///   artifact of this one binary, not a portable fact). A PHP-minor pin cannot
///   see the linked libcurl version a deployed project will run against, so
///   `int|false` — the nsrt-measured shape — is the honest floor.
/// * **Every list/array-shaped option** (`CURLINFO_CERTINFO`,
///   `CURLINFO_SSL_ENGINES`, `CURLINFO_COOKIELIST`) and **the zero- or
///   one-argument whole-array form** — out of this rung's scope; a shape-typed
///   result is a different rule (`shape_projection_fact`'s family), not this
///   scalar table's.
/// * **A non-constant, or unrecognized-name, `$option`** — the table has
///   nothing to key on.
pub(super) fn curl_getinfo_transfer(args: &[ArgValue]) -> Option<Fact> {
    let [_handle, option] = args else { return None };
    let ArgValue::GlobalConst(r) = option else { return None };
    // Constants are case-sensitive (unlike PHP's function/class names), so the
    // match below is exact. A `Qualified`/`Relative` spelling never denotes the
    // global `CURLINFO_*` constant — the same `FullyQualified`/`Unqualified`
    // split `cond.rs`'s `PHP_VERSION_ID` identity check applies (issue #29).
    if !matches!(r.kind, RefKind::FullyQualified | RefKind::Unqualified) {
        return None;
    }

    /// The 37 constants documented (and probed) as a plain `int`.
    const INT: &[&str] = &[
        "CURLINFO_FILETIME",
        "CURLINFO_REDIRECT_COUNT",
        "CURLINFO_PRIMARY_PORT",
        "CURLINFO_LOCAL_PORT",
        "CURLINFO_HEADER_SIZE",
        "CURLINFO_REQUEST_SIZE",
        "CURLINFO_SSL_VERIFYRESULT",
        "CURLINFO_RESPONSE_CODE",
        "CURLINFO_HTTP_CODE",
        "CURLINFO_HTTP_CONNECTCODE",
        "CURLINFO_HTTPAUTH_AVAIL",
        "CURLINFO_PROXYAUTH_AVAIL",
        "CURLINFO_OS_ERRNO",
        "CURLINFO_NUM_CONNECTS",
        "CURLINFO_CONDITION_UNMET",
        "CURLINFO_RTSP_CLIENT_CSEQ",
        "CURLINFO_RTSP_CSEQ_RECV",
        "CURLINFO_RTSP_SERVER_CSEQ",
        // The PHP 7.3+ `_T` (microsecond-precision) family.
        "CURLINFO_CONTENT_LENGTH_DOWNLOAD_T",
        "CURLINFO_CONTENT_LENGTH_UPLOAD_T",
        "CURLINFO_HTTP_VERSION",
        "CURLINFO_PROTOCOL",
        "CURLINFO_PROXY_SSL_VERIFYRESULT",
        "CURLINFO_SIZE_DOWNLOAD_T",
        "CURLINFO_SIZE_UPLOAD_T",
        "CURLINFO_SPEED_DOWNLOAD_T",
        "CURLINFO_SPEED_UPLOAD_T",
        "CURLINFO_APPCONNECT_TIME_T",
        "CURLINFO_CONNECT_TIME_T",
        "CURLINFO_FILETIME_T",
        "CURLINFO_NAMELOOKUP_TIME_T",
        "CURLINFO_PRETRANSFER_TIME_T",
        "CURLINFO_REDIRECT_TIME_T",
        "CURLINFO_STARTTRANSFER_TIME_T",
        "CURLINFO_TOTAL_TIME_T",
        // PHP 8.2+.
        "CURLINFO_PROXY_ERROR",
        "CURLINFO_RETRY_AFTER",
    ];
    /// The 13 constants documented (and probed) as a plain `float`.
    const FLOAT: &[&str] = &[
        "CURLINFO_TOTAL_TIME",
        "CURLINFO_NAMELOOKUP_TIME",
        "CURLINFO_CONNECT_TIME",
        "CURLINFO_PRETRANSFER_TIME",
        "CURLINFO_STARTTRANSFER_TIME",
        "CURLINFO_REDIRECT_TIME",
        "CURLINFO_SIZE_UPLOAD",
        "CURLINFO_SIZE_DOWNLOAD",
        "CURLINFO_SPEED_DOWNLOAD",
        "CURLINFO_SPEED_UPLOAD",
        "CURLINFO_CONTENT_LENGTH_DOWNLOAD",
        "CURLINFO_CONTENT_LENGTH_UPLOAD",
        "CURLINFO_APPCONNECT_TIME",
    ];
    /// The 4 constants probed as an unconditional (never-`false`) `string`: each
    /// coalesces an unset field to `''`, not `false` — see the module doc above
    /// for the measurement that sets these apart from `CURLINFO_SCHEME`.
    const STRING: &[&str] = &[
        "CURLINFO_EFFECTIVE_URL",
        "CURLINFO_PRIMARY_IP",
        "CURLINFO_LOCAL_IP",
        "CURLINFO_EFFECTIVE_METHOD",
    ];

    let name = r.raw.as_str();
    if INT.contains(&name) {
        Some(Fact::General { base: Base::Int, nullable: false })
    } else if FLOAT.contains(&name) {
        Some(Fact::General { base: Base::Float, nullable: false })
    } else if STRING.contains(&name) {
        Some(Fact::General { base: Base::String, nullable: false })
    } else {
        None
    }
}
