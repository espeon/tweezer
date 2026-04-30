use crate::TweezerError;

/// Error when parsing command arguments into typed values.
#[derive(Debug, Clone, PartialEq)]
pub enum ParseArgsError {
    MissingArgument(usize),
    InvalidValue {
        index: usize,
        value: String,
        expected: &'static str,
    },
    TooManyArguments,
}

impl std::fmt::Display for ParseArgsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseArgsError::MissingArgument(idx) => {
                write!(f, "missing argument at position {idx}")
            }
            ParseArgsError::InvalidValue {
                index,
                value,
                expected,
            } => {
                write!(f, "cannot parse '{value}' as {expected} (position {index})")
            }
            ParseArgsError::TooManyArguments => {
                write!(f, "too many arguments provided")
            }
        }
    }
}

impl std::error::Error for ParseArgsError {}

impl From<ParseArgsError> for TweezerError {
    fn from(e: ParseArgsError) -> Self {
        TweezerError::Handler(e.to_string())
    }
}

/// Parse typed values from a slice of string arguments.
///
/// Implementations are provided for primitives, `String`, `Option<T>`,
/// tuples (up to 12 elements), and `Vec<T>`.
///
/// # Example
/// ```rust,ignore
/// let (name, count): (String, u32) = ctx.parse_args()?;
/// ```
pub trait FromArgs: Sized {
    fn from_args(args: &[String]) -> Result<(Self, &[String]), ParseArgsError>;
}

// ---------------------------------------------------------------------------
// Primitives
// ---------------------------------------------------------------------------

macro_rules! impl_from_args_int {
    ($ty:ty, $name:expr) => {
        impl FromArgs for $ty {
            fn from_args(args: &[String]) -> Result<(Self, &[String]), ParseArgsError> {
                let val = args.first().ok_or(ParseArgsError::MissingArgument(0))?;
                let parsed = val
                    .parse()
                    .map_err(|_| ParseArgsError::InvalidValue {
                        index: 0,
                        value: val.clone(),
                        expected: $name,
                    })?;
                Ok((parsed, &args[1..]))
            }
        }
    };
}

impl_from_args_int!(u8, "u8");
impl_from_args_int!(u16, "u16");
impl_from_args_int!(u32, "u32");
impl_from_args_int!(u64, "u64");
impl_from_args_int!(i8, "i8");
impl_from_args_int!(i16, "i16");
impl_from_args_int!(i32, "i32");
impl_from_args_int!(i64, "i64");
impl_from_args_int!(usize, "usize");
impl_from_args_int!(isize, "isize");

macro_rules! impl_from_args_float {
    ($ty:ty, $name:expr) => {
        impl FromArgs for $ty {
            fn from_args(args: &[String]) -> Result<(Self, &[String]), ParseArgsError> {
                let val = args.first().ok_or(ParseArgsError::MissingArgument(0))?;
                let parsed = val
                    .parse()
                    .map_err(|_| ParseArgsError::InvalidValue {
                        index: 0,
                        value: val.clone(),
                        expected: $name,
                    })?;
                Ok((parsed, &args[1..]))
            }
        }
    };
}

impl_from_args_float!(f32, "f32");
impl_from_args_float!(f64, "f64");

impl FromArgs for String {
    fn from_args(args: &[String]) -> Result<(Self, &[String]), ParseArgsError> {
        let val = args
            .first()
            .ok_or(ParseArgsError::MissingArgument(0))?
            .clone();
        Ok((val, &args[1..]))
    }
}

impl FromArgs for bool {
    fn from_args(args: &[String]) -> Result<(Self, &[String]), ParseArgsError> {
        let val = args.first().ok_or(ParseArgsError::MissingArgument(0))?;
        let parsed = match val.to_lowercase().as_str() {
            "true" | "yes" | "1" | "on" => true,
            "false" | "no" | "0" | "off" => false,
            _ => {
                return Err(ParseArgsError::InvalidValue {
                    index: 0,
                    value: val.clone(),
                    expected: "bool",
                })
            }
        };
        Ok((parsed, &args[1..]))
    }
}

// ---------------------------------------------------------------------------
// Option<T> — consumes if available, never fails
// ---------------------------------------------------------------------------

impl<T: FromArgs> FromArgs for Option<T> {
    fn from_args(args: &[String]) -> Result<(Self, &[String]), ParseArgsError> {
        match T::from_args(args) {
            Ok((val, rest)) => Ok((Some(val), rest)),
            Err(ParseArgsError::MissingArgument(_)) => Ok((None, args)),
            Err(e) => Err(e),
        }
    }
}

// ---------------------------------------------------------------------------
// Vec<T> — consumes all remaining arguments
// ---------------------------------------------------------------------------

impl<T: FromArgs> FromArgs for Vec<T> {
    fn from_args(args: &[String]) -> Result<(Self, &[String]), ParseArgsError> {
        let mut result = Vec::new();
        let mut remaining = args;
        while !remaining.is_empty() {
            match T::from_args(remaining) {
                Ok((val, rest)) => {
                    result.push(val);
                    remaining = rest;
                }
                Err(ParseArgsError::MissingArgument(_)) => break,
                Err(e) => return Err(e),
            }
        }
        Ok((result, remaining))
    }
}

// ---------------------------------------------------------------------------
// Tuples — parse each element in order
// ---------------------------------------------------------------------------

impl<T1: FromArgs> FromArgs for (T1,) {
    fn from_args(args: &[String]) -> Result<(Self, &[String]), ParseArgsError> {
        let (t1, rest) = T1::from_args(args)?;
        Ok(((t1,), rest))
    }
}

impl<T1: FromArgs, T2: FromArgs> FromArgs for (T1, T2) {
    fn from_args(args: &[String]) -> Result<(Self, &[String]), ParseArgsError> {
        let (t1, rest) = T1::from_args(args)?;
        let (t2, rest) = T2::from_args(rest)?;
        Ok(((t1, t2), rest))
    }
}

impl<T1: FromArgs, T2: FromArgs, T3: FromArgs> FromArgs for (T1, T2, T3) {
    fn from_args(args: &[String]) -> Result<(Self, &[String]), ParseArgsError> {
        let (t1, rest) = T1::from_args(args)?;
        let (t2, rest) = T2::from_args(rest)?;
        let (t3, rest) = T3::from_args(rest)?;
        Ok(((t1, t2, t3), rest))
    }
}

impl<T1: FromArgs, T2: FromArgs, T3: FromArgs, T4: FromArgs> FromArgs for (T1, T2, T3, T4) {
    fn from_args(args: &[String]) -> Result<(Self, &[String]), ParseArgsError> {
        let (t1, rest) = T1::from_args(args)?;
        let (t2, rest) = T2::from_args(rest)?;
        let (t3, rest) = T3::from_args(rest)?;
        let (t4, rest) = T4::from_args(rest)?;
        Ok(((t1, t2, t3, t4), rest))
    }
}

impl<T1: FromArgs, T2: FromArgs, T3: FromArgs, T4: FromArgs, T5: FromArgs> FromArgs
    for (T1, T2, T3, T4, T5)
{
    fn from_args(args: &[String]) -> Result<(Self, &[String]), ParseArgsError> {
        let (t1, rest) = T1::from_args(args)?;
        let (t2, rest) = T2::from_args(rest)?;
        let (t3, rest) = T3::from_args(rest)?;
        let (t4, rest) = T4::from_args(rest)?;
        let (t5, rest) = T5::from_args(rest)?;
        Ok(((t1, t2, t3, t4, t5), rest))
    }
}

impl<T1: FromArgs, T2: FromArgs, T3: FromArgs, T4: FromArgs, T5: FromArgs, T6: FromArgs> FromArgs
    for (T1, T2, T3, T4, T5, T6)
{
    fn from_args(args: &[String]) -> Result<(Self, &[String]), ParseArgsError> {
        let (t1, rest) = T1::from_args(args)?;
        let (t2, rest) = T2::from_args(rest)?;
        let (t3, rest) = T3::from_args(rest)?;
        let (t4, rest) = T4::from_args(rest)?;
        let (t5, rest) = T5::from_args(rest)?;
        let (t6, rest) = T6::from_args(rest)?;
        Ok(((t1, t2, t3, t4, t5, t6), rest))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_string() {
        let args = a(&["hello", "world"]);
        let (val, rest) = String::from_args(&args).unwrap();
        assert_eq!(val, "hello");
        assert_eq!(rest, &["world"][..]);
    }

    #[test]
    fn parse_u32() {
        let args = a(&["42", "extra"]);
        let (val, rest) = u32::from_args(&args).unwrap();
        assert_eq!(val, 42);
        assert_eq!(rest, &["extra"][..]);
    }

    #[test]
    fn parse_u32_invalid() {
        let args = a(&["abc"]);
        let err = u32::from_args(&args).unwrap_err();
        assert_eq!(
            err,
            ParseArgsError::InvalidValue {
                index: 0,
                value: "abc".into(),
                expected: "u32",
            }
        );
    }

    #[test]
    fn parse_missing() {
        let err = String::from_args(&[]).unwrap_err();
        assert_eq!(err, ParseArgsError::MissingArgument(0));
    }

    #[test]
    fn parse_bool_variants() {
        for s in &["true", "TRUE", "yes", "1", "on"] {
            let args = a(&[s]);
            let (val, _) = bool::from_args(&args).unwrap();
            assert!(val, "failed for {s}");
        }
        for s in &["false", "FALSE", "no", "0", "off"] {
            let args = a(&[s]);
            let (val, _) = bool::from_args(&args).unwrap();
            assert!(!val, "failed for {s}");
        }
    }

    #[test]
    fn parse_bool_invalid() {
        let args = a(&["maybe"]);
        let err = bool::from_args(&args).unwrap_err();
        assert!(
            matches!(err, ParseArgsError::InvalidValue { expected: "bool", .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn parse_tuple_2() {
        let args = a(&["hello", "42"]);
        let (val, rest): ((String, u32), _) = FromArgs::from_args(&args).unwrap();
        assert_eq!(val.0, "hello");
        assert_eq!(val.1, 42);
        assert!(rest.is_empty());
    }

    #[test]
    fn parse_tuple_3() {
        let args = a(&["hi", "7", "true"]);
        let (val, rest): ((String, u32, bool), _) = FromArgs::from_args(&args).unwrap();
        assert_eq!(val.0, "hi");
        assert_eq!(val.1, 7);
        assert!(val.2);
        assert!(rest.is_empty());
    }

    #[test]
    fn parse_option_some() {
        let args = a(&["42", "extra"]);
        let (val, rest): (Option<u32>, _) = FromArgs::from_args(&args).unwrap();
        assert_eq!(val, Some(42));
        assert_eq!(rest, &["extra"][..]);
    }

    #[test]
    fn parse_option_none() {
        let (val, rest): (Option<u32>, _) = FromArgs::from_args(&[]).unwrap();
        assert_eq!(val, None);
        assert!(rest.is_empty());
    }

    #[test]
    fn parse_option_invalid() {
        let args = a(&["abc"]);
        let err: Result<(Option<u32>, _), _> = FromArgs::from_args(&args);
        assert!(
            matches!(err, Err(ParseArgsError::InvalidValue { expected: "u32", .. })),
            "got {err:?}"
        );
    }

    #[test]
    fn parse_vec_all() {
        let args = a(&["1", "2", "3"]);
        let (val, rest): (Vec<u32>, _) = FromArgs::from_args(&args).unwrap();
        assert_eq!(val, vec![1, 2, 3]);
        assert!(rest.is_empty());
    }

    #[test]
    fn parse_vec_empty() {
        let (val, rest): (Vec<u32>, _) = FromArgs::from_args(&[]).unwrap();
        assert!(val.is_empty());
        assert!(rest.is_empty());
    }

    #[test]
    fn parse_tuple_with_option_and_vec() {
        let args = a(&["cmd", "42", "a", "b"]);
        let (val, rest): ((String, Option<u32>, Vec<String>), _) =
            FromArgs::from_args(&args).unwrap();
        assert_eq!(val.0, "cmd");
        assert_eq!(val.1, Some(42));
        assert_eq!(val.2, vec!["a", "b"]);
        assert!(rest.is_empty());
    }

    #[test]
    fn parse_tuple_with_missing_optional() {
        let args = a(&["cmd"]);
        let (val, rest): ((String, Option<u32>, Vec<String>), _) =
            FromArgs::from_args(&args).unwrap();
        assert_eq!(val.0, "cmd");
        assert_eq!(val.1, None);
        assert!(val.2.is_empty());
        assert!(rest.is_empty());
    }

    #[test]
    fn parse_f64() {
        let args = a(&["3.14"]);
        let (val, rest) = f64::from_args(&args).unwrap();
        assert!((val - 3.14).abs() < 0.001);
        assert!(rest.is_empty());
    }

    #[test]
    fn parse_isize() {
        let args = a(&["-7"]);
        let (val, _) = isize::from_args(&args).unwrap();
        assert_eq!(val, -7);
    }
}
