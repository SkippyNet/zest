//! A small, read-only window onto WMI.
//!
//! Queries return plain [`Record`]s (property name to value), so nothing that
//! touches COM leaves this module and the code that interprets the answers
//! can be tested with hand-made records.

use std::collections::HashMap;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
    CoSetProxyBlanket, CoUninitialize, EOAC_NONE, RPC_C_AUTHN_LEVEL_PKT_PRIVACY,
    RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::Win32::System::Variant::{
    VARIANT, VT_BOOL, VT_BSTR, VT_EMPTY, VT_I1, VT_I2, VT_I4, VT_I8, VT_INT, VT_NULL, VT_R4, VT_R8,
    VT_UI1, VT_UI2, VT_UI4, VT_UI8, VT_UINT,
};
use windows::Win32::System::Wmi::{
    IWbemClassObject, IWbemLocator, IWbemServices, WBEM_FLAG_FORWARD_ONLY,
    WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_GENERIC_FLAG_TYPE, WbemLocator,
};
use windows::core::{BSTR, PCWSTR};

/// `RPC_C_AUTHN_WINNT`
const RPC_C_AUTHN_WINNT: u32 = 10;
/// `RPC_C_AUTHZ_NONE`
const RPC_C_AUTHZ_NONE: u32 = 0;
/// WMI: access denied.
const WBEM_E_ACCESS_DENIED: u32 = 0x8004_1003;
/// Win32: access denied, as an HRESULT.
const E_ACCESSDENIED: u32 = 0x8007_0005;
/// `WBEM_S_TIMEDOUT`: the enumerator had nothing ready within the wait.
const WBEM_S_TIMEDOUT: i32 = 0x0004_0004;
/// Longest wait for the next row of an answer, in milliseconds. A healthy
/// provider answers in well under a second; a broken one must not hang the page.
const ROW_TIMEOUT_MS: i32 = 8_000;

/// Why a WMI query gave no answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WmiError {
    /// The class can only be read with administrator rights.
    AccessDenied,
    /// The namespace, class or provider is missing or failed.
    Unavailable(String),
}

impl WmiError {
    fn from_windows(e: &windows::core::Error) -> WmiError {
        match e.code().0 as u32 {
            WBEM_E_ACCESS_DENIED | E_ACCESSDENIED => WmiError::AccessDenied,
            _ => WmiError::Unavailable(e.message()),
        }
    }
}

/// One property value, reduced to what the inventory needs.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Text(String),
    Int(i64),
    Real(f64),
    Bool(bool),
}

/// One row of a query answer: property name (as asked for) to value.
/// Properties that are null or of an unsupported type are simply absent.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Record(HashMap<String, Value>);

impl Record {
    /// Builds a record from literal values (used by tests and demo data).
    #[cfg(test)]
    pub(crate) fn from_pairs(pairs: &[(&str, Value)]) -> Record {
        Record(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        )
    }

    /// The property as trimmed text; `None` when missing or blank.
    pub(crate) fn text(&self, name: &str) -> Option<String> {
        let text = match self.0.get(name)? {
            Value::Text(t) => t.trim().to_owned(),
            Value::Int(n) => n.to_string(),
            Value::Real(r) => r.to_string(),
            Value::Bool(b) => b.to_string(),
        };
        (!text.is_empty()).then_some(text)
    }

    /// The property as an unsigned number. WMI hands 64-bit integers over as
    /// text, so text that parses as a number counts too.
    pub(crate) fn u64(&self, name: &str) -> Option<u64> {
        match self.0.get(name)? {
            Value::Int(n) => u64::try_from(*n).ok(),
            Value::Text(t) => t.trim().parse().ok(),
            Value::Real(r) if *r >= 0.0 => Some(*r as u64),
            _ => None,
        }
    }

    /// The property as a 32-bit unsigned number.
    pub(crate) fn u32(&self, name: &str) -> Option<u32> {
        self.u64(name).and_then(|n| u32::try_from(n).ok())
    }

    /// The property as a single character. WMI hands `char16` values over
    /// as numbers; zero means "none".
    pub(crate) fn char(&self, name: &str) -> Option<char> {
        match self.0.get(name)? {
            Value::Int(code) => u32::try_from(*code)
                .ok()
                .filter(|&code| code != 0)
                .and_then(char::from_u32),
            Value::Text(text) => {
                let mut chars = text.trim().chars();
                let first = chars.next()?;
                chars.next().is_none().then_some(first)
            }
            _ => None,
        }
    }

    /// The property as a truth value.
    pub(crate) fn bool(&self, name: &str) -> Option<bool> {
        match self.0.get(name)? {
            Value::Bool(b) => Some(*b),
            Value::Int(n) => Some(*n != 0),
            Value::Text(t) => match t.trim().to_ascii_lowercase().as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            },
            Value::Real(_) => None,
        }
    }
}

/// Joins the multithreaded COM apartment for as long as it lives; a thread
/// that already belongs to another apartment is left as it is.
struct ComGuard {
    uninitialize: bool,
}

impl ComGuard {
    fn new() -> ComGuard {
        // SAFETY: plain COM initialisation, balanced in `Drop` only when this
        // call actually added a reference.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        ComGuard {
            uninitialize: hr.is_ok(),
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.uninitialize {
            // SAFETY: balances the successful CoInitializeEx in `new`.
            unsafe { CoUninitialize() };
        }
    }
}

/// An open connection to one WMI namespace.
pub(crate) struct Wmi {
    // Declared before `_com` so it is released while COM is still up.
    services: IWbemServices,
    _com: ComGuard,
}

impl Wmi {
    /// Connects to a namespace such as `ROOT\CIMV2`.
    pub(crate) fn connect(namespace: &str) -> Result<Wmi, WmiError> {
        let com = ComGuard::new();
        // SAFETY: the standard WMI connection sequence on valid interfaces,
        // on a thread whose COM apartment `com` keeps alive.
        let services = unsafe {
            let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| WmiError::from_windows(&e))?;
            let services = locator
                .ConnectServer(
                    &BSTR::from(namespace),
                    &BSTR::new(),
                    &BSTR::new(),
                    &BSTR::new(),
                    0,
                    &BSTR::new(),
                    None,
                )
                .map_err(|e| WmiError::from_windows(&e))?;
            CoSetProxyBlanket(
                &services,
                RPC_C_AUTHN_WINNT,
                RPC_C_AUTHZ_NONE,
                PCWSTR::null(),
                RPC_C_AUTHN_LEVEL_PKT_PRIVACY,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
            )
            .map_err(|e| WmiError::from_windows(&e))?;
            services
        };
        Ok(Wmi {
            services,
            _com: com,
        })
    }

    /// Reads the named properties of every instance of `class`, optionally
    /// narrowed by a WQL `WHERE` condition.
    ///
    /// Only the listed properties are requested: several classes compute
    /// expensive values on demand, and others hold identifiers (serial
    /// numbers, addresses) this crate has no business reading.
    pub(crate) fn query(
        &self,
        class: &str,
        properties: &[&str],
        condition: Option<&str>,
    ) -> Result<Vec<Record>, WmiError> {
        let mut wql = format!("SELECT {} FROM {class}", properties.join(", "));
        if let Some(condition) = condition {
            wql.push_str(" WHERE ");
            wql.push_str(condition);
        }
        let mut records = Vec::new();
        // SAFETY: COM stays initialised for the life of `self`; every
        // interface used here is dropped before this function returns.
        unsafe {
            let enumerator = self
                .services
                .ExecQuery(
                    &BSTR::from("WQL"),
                    &BSTR::from(wql.as_str()),
                    WBEM_GENERIC_FLAG_TYPE(
                        WBEM_FLAG_FORWARD_ONLY.0 | WBEM_FLAG_RETURN_IMMEDIATELY.0,
                    ),
                    None,
                )
                .map_err(|e| WmiError::from_windows(&e))?;
            loop {
                let mut row = [None];
                let mut returned = 0u32;
                let status = enumerator.Next(ROW_TIMEOUT_MS, &mut row, &mut returned);
                if status.is_err() {
                    return Err(WmiError::from_windows(&status.into()));
                }
                if status.0 == WBEM_S_TIMEDOUT {
                    return Err(WmiError::Unavailable(format!(
                        "{class} did not answer in time"
                    )));
                }
                let Some(object) = row[0].take().filter(|_| returned == 1) else {
                    break;
                };
                records.push(read_record(&object, properties));
            }
        }
        Ok(records)
    }
}

/// Copies the requested properties out of a WMI object.
///
/// # Safety
/// Must be called on a thread with COM initialised.
unsafe fn read_record(object: &IWbemClassObject, properties: &[&str]) -> Record {
    let mut record = HashMap::with_capacity(properties.len());
    for name in properties {
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let mut variant = VARIANT::default();
        // SAFETY: `wide` is NUL terminated and `variant` is a valid out parameter.
        let read = unsafe { object.Get(PCWSTR(wide.as_ptr()), 0, &mut variant, None, None) };
        if read.is_err() {
            continue;
        }
        if let Some(value) = value_of(&variant) {
            record.insert((*name).to_owned(), value);
        }
    }
    Record(record)
}

/// Reduces a variant to a [`Value`]; null, arrays and objects give `None`.
fn value_of(variant: &VARIANT) -> Option<Value> {
    let vt = variant.vt();
    if vt == VT_NULL || vt == VT_EMPTY {
        None
    } else if vt == VT_BOOL {
        bool::try_from(variant).ok().map(Value::Bool)
    } else if vt == VT_BSTR {
        BSTR::try_from(variant)
            .ok()
            .map(|text| Value::Text(text.to_string()))
    } else if [VT_I1, VT_I2, VT_I4, VT_I8, VT_INT].contains(&vt) {
        i64::try_from(variant).ok().map(Value::Int)
    } else if [VT_UI1, VT_UI2, VT_UI4, VT_UINT].contains(&vt) {
        u32::try_from(variant)
            .ok()
            .map(|n| Value::Int(i64::from(n)))
    } else if vt == VT_UI8 {
        u64::try_from(variant)
            .ok()
            .and_then(|n| i64::try_from(n).ok())
            .map(Value::Int)
    } else if vt == VT_R4 || vt == VT_R8 {
        f64::try_from(variant).ok().map(Value::Real)
    } else {
        None
    }
}

/// Parses the date part of a WMI timestamp (`yyyymmddHHMMSS.mmmmmm±UUU`).
pub(crate) fn parse_date(text: &str) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(text.get(..8)?, "%Y%m%d").ok()
}

/// Parses a WMI timestamp into local time. The trailing offset is the
/// distance from UTC in minutes.
pub(crate) fn parse_time(text: &str) -> Option<chrono::NaiveDateTime> {
    let naive = chrono::NaiveDateTime::parse_from_str(text.get(..14)?, "%Y%m%d%H%M%S").ok()?;
    let offset_minutes: i64 = text
        .get(21..)
        .and_then(|o| o.trim().parse().ok())
        .unwrap_or(0);
    let utc = naive.checked_sub_signed(chrono::Duration::minutes(offset_minutes))?;
    Some(utc.and_utc().with_timezone(&chrono::Local).naive_local())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_convert_between_the_shapes_wmi_uses() {
        let record = Record::from_pairs(&[
            ("Name", Value::Text("  Example  CPU ".to_owned())),
            ("Blank", Value::Text("   ".to_owned())),
            ("Size", Value::Text("1024209543168".to_owned())),
            ("Cores", Value::Int(10)),
            ("Negative", Value::Int(-1)),
            ("Flag", Value::Bool(true)),
            ("FlagAsNumber", Value::Int(0)),
            ("FlagAsText", Value::Text("True".to_owned())),
        ]);
        assert_eq!(record.text("Name").as_deref(), Some("Example  CPU"));
        assert_eq!(record.text("Blank"), None);
        assert_eq!(record.text("Missing"), None);
        assert_eq!(record.u64("Size"), Some(1_024_209_543_168));
        assert_eq!(record.u32("Size"), None, "too large for 32 bits");
        assert_eq!(record.u32("Cores"), Some(10));
        assert_eq!(record.u64("Negative"), None);
        assert_eq!(record.u64("Name"), None);
        assert_eq!(record.bool("Flag"), Some(true));
        assert_eq!(record.bool("FlagAsNumber"), Some(false));
        assert_eq!(record.bool("FlagAsText"), Some(true));
        assert_eq!(record.bool("Name"), None);
    }

    #[test]
    fn timestamps() {
        let date = parse_date("20241211000000.000000+000").unwrap();
        assert_eq!(date.to_string(), "2024-12-11");
        assert_eq!(parse_date("2024"), None);
        assert_eq!(parse_date("not a date at all"), None);
        // The offset is applied, so the same instant parses to the same local
        // time whichever zone it was written in.
        let utc = parse_time("20260101120000.000000+000").unwrap();
        let plus_two = parse_time("20260101140000.000000+120").unwrap();
        assert_eq!(utc, plus_two);
        assert_eq!(parse_time("garbage"), None);
    }

    #[test]
    fn queries_the_local_machine() {
        let wmi = Wmi::connect(r"ROOT\CIMV2").expect("the base namespace always exists");
        let rows = wmi
            .query("Win32_OperatingSystem", &["Caption", "BuildNumber"], None)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].text("Caption").unwrap().contains("Windows"));
        assert!(rows[0].u32("BuildNumber").unwrap() > 10_000);

        assert!(matches!(
            wmi.query("Zest_NoSuchClass", &["Name"], None),
            Err(WmiError::Unavailable(_))
        ));
        assert!(matches!(
            Wmi::connect(r"ROOT\ZestNoSuchNamespace"),
            Err(WmiError::Unavailable(_))
        ));
    }
}
