//! Compatibility semantics for temporal function arguments.
//!
//! sqlparser 0.62 retains date-part arguments of generic `Function` nodes as
//! ordinary `Expr::Identifier` values, which creates a semantic gap for
//! lineage. This small, profile-scoped layer bridges that gap until sqlparser
//! provides typed temporal function arguments; at that point these profiles
//! and the registry should be removed or migrated to the typed AST. The
//! approach follows the direction discussed in [PR 1191](https://github.com/apache/datafusion-sqlparser-rs/pull/1191)
//! (including `BigQuery` `WEEK(MONDAY)`) and [issue 1983](https://github.com/apache/datafusion-sqlparser-rs/issues/1983)
//! / [PR 2030](https://github.com/apache/datafusion-sqlparser-rs/pull/2030)
//! (dialect-specific `EXTRACT` date-part parsing).
//!
//! The tables below are an exception inventory for official static syntax that
//! sqlparser represents as generic identifiers, not a registry of every
//! temporal function. Unknown functions and dynamic expressions retain the
//! generic all-values fallback.

use sqlparser::ast::{
    Expr, Function, FunctionArg, FunctionArgExpr, FunctionArguments, ObjectNamePart,
};

use crate::types::Dialect;

/// The lineage role of an argument in a known function grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArgumentSemantic {
    ValueExpression,
    DatePart(DatePartGrammar),
}

/// A dialect-specific function signature. This deliberately lives outside
/// sqlparser's AST: the AST describes syntax, while this layer describes
/// which syntax contributes data lineage.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FunctionSignature {
    pub names: &'static [&'static str],
    pub arity: usize,
    pub arguments: &'static [ArgumentSemantic],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DatePartGrammar {
    profile: &'static DatePartProfile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DatePartToken {
    canonical: &'static str,
    aliases: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DatePartProfile {
    token_groups: &'static [&'static [DatePartToken]],
    allows_weekday_modifier: bool,
}

const BQ_VALUE_PART: &[ArgumentSemantic] = &[
    ArgumentSemantic::ValueExpression,
    ArgumentSemantic::DatePart(BIGQUERY_DATE_PART),
];
const BQ_VALUE_PART_VALUE: &[ArgumentSemantic] = &[
    ArgumentSemantic::ValueExpression,
    ArgumentSemantic::DatePart(BIGQUERY_DATE_PART),
    ArgumentSemantic::ValueExpression,
];
const BQ_VALUE_VALUE_PART: &[ArgumentSemantic] = &[
    ArgumentSemantic::ValueExpression,
    ArgumentSemantic::ValueExpression,
    ArgumentSemantic::DatePart(BIGQUERY_DATE_PART),
];
const BQ_VALUE: &[ArgumentSemantic] = &[ArgumentSemantic::ValueExpression];
const SNOW_PART_VALUE_VALUE: &[ArgumentSemantic] = &[
    ArgumentSemantic::DatePart(SNOWFLAKE_DATE_PART),
    ArgumentSemantic::ValueExpression,
    ArgumentSemantic::ValueExpression,
];
const SNOW_PART_VALUE: &[ArgumentSemantic] = &[
    ArgumentSemantic::DatePart(SNOWFLAKE_DATE_PART),
    ArgumentSemantic::ValueExpression,
];
const SNOW_VALUE_PART: &[ArgumentSemantic] = &[
    ArgumentSemantic::ValueExpression,
    ArgumentSemantic::DatePart(SNOWFLAKE_DATE_PART),
];
const DB_PART_VALUE_VALUE: &[ArgumentSemantic] = &[
    ArgumentSemantic::DatePart(DATABRICKS_DATE_PART),
    ArgumentSemantic::ValueExpression,
    ArgumentSemantic::ValueExpression,
];
const DB_DIFF_PART_VALUE_VALUE: &[ArgumentSemantic] = &[
    ArgumentSemantic::DatePart(DATABRICKS_DIFF_DATE_PART),
    ArgumentSemantic::ValueExpression,
    ArgumentSemantic::ValueExpression,
];
const MYSQL_PART_VALUE_VALUE: &[ArgumentSemantic] = &[
    ArgumentSemantic::DatePart(MYSQL_DATE_PART),
    ArgumentSemantic::ValueExpression,
    ArgumentSemantic::ValueExpression,
];
const REDSHIFT_PART_VALUE_VALUE: &[ArgumentSemantic] = &[
    ArgumentSemantic::DatePart(REDSHIFT_ADD_DIFF_DATE_PART),
    ArgumentSemantic::ValueExpression,
    ArgumentSemantic::ValueExpression,
];
const REDSHIFT_PART_VALUE: &[ArgumentSemantic] = &[
    ArgumentSemantic::DatePart(REDSHIFT_DATE_PART),
    ArgumentSemantic::ValueExpression,
];
const MSSQL_PART_VALUE_VALUE: &[ArgumentSemantic] = &[
    ArgumentSemantic::DatePart(MSSQL_DATEADD_DATE_PART),
    ArgumentSemantic::ValueExpression,
    ArgumentSemantic::ValueExpression,
];
const MSSQL_DATEDIFF_PART_VALUE_VALUE: &[ArgumentSemantic] = &[
    ArgumentSemantic::DatePart(MSSQL_DATEDIFF_DATE_PART),
    ArgumentSemantic::ValueExpression,
    ArgumentSemantic::ValueExpression,
];
const MSSQL_PART_VALUE: &[ArgumentSemantic] = &[
    ArgumentSemantic::DatePart(MSSQL_DATEPART_DATE_PART),
    ArgumentSemantic::ValueExpression,
];
const MSSQL_DATETRUNC_PART_VALUE: &[ArgumentSemantic] = &[
    ArgumentSemantic::DatePart(MSSQL_DATETRUNC_DATE_PART),
    ArgumentSemantic::ValueExpression,
];
const MSSQL_DATE_BUCKET_PART_VALUE_VALUE: &[ArgumentSemantic] = &[
    ArgumentSemantic::DatePart(MSSQL_DATE_BUCKET_DATE_PART),
    ArgumentSemantic::ValueExpression,
    ArgumentSemantic::ValueExpression,
];
const MSSQL_DATE_BUCKET_PART_VALUE_VALUE_VALUE: &[ArgumentSemantic] = &[
    ArgumentSemantic::DatePart(MSSQL_DATE_BUCKET_DATE_PART),
    ArgumentSemantic::ValueExpression,
    ArgumentSemantic::ValueExpression,
    ArgumentSemantic::ValueExpression,
];

const EMPTY_ALIASES: &[&str] = &[];
const BQ_PARTS: &[DatePartToken] = &[
    DatePartToken {
        canonical: "MICROSECOND",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "MILLISECOND",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "SECOND",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "MINUTE",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "HOUR",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "DAY",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "WEEK",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "ISOWEEK",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "MONTH",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "QUARTER",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "YEAR",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "ISOYEAR",
        aliases: EMPTY_ALIASES,
    },
];
const SNOWFLAKE_PARTS: &[DatePartToken] = &[
    DatePartToken {
        canonical: "YEAR",
        aliases: &["y", "yy", "yyy", "yyyy", "yr", "years", "yrs"],
    },
    DatePartToken {
        canonical: "QUARTER",
        aliases: &["q", "qtr", "qtrs", "quarters"],
    },
    DatePartToken {
        canonical: "MONTH",
        aliases: &["mm", "mon", "mons", "months"],
    },
    DatePartToken {
        canonical: "DAY",
        aliases: &["d", "dd", "days", "dayofmonth"],
    },
    DatePartToken {
        canonical: "DAYOFWEEK",
        aliases: &["weekday", "dow", "dw"],
    },
    DatePartToken {
        canonical: "DAYOFWEEKISO",
        aliases: &["weekday_iso", "dow_iso", "dw_iso", "dayofweek_iso"],
    },
    DatePartToken {
        canonical: "DAYOFYEAR",
        aliases: &["doy", "dy", "yearday"],
    },
    DatePartToken {
        canonical: "WEEK",
        aliases: &["w", "wk", "ww", "weekofyear", "woy", "wy"],
    },
    DatePartToken {
        canonical: "WEEKISO",
        aliases: &["isoweek", "week_iso", "weekofyeariso", "weekofyear_iso"],
    },
    DatePartToken {
        canonical: "HOUR",
        aliases: &["h", "hh", "hr", "hours", "hrs"],
    },
    DatePartToken {
        canonical: "MINUTE",
        aliases: &["m", "mi", "min", "minutes", "mins"],
    },
    DatePartToken {
        canonical: "SECOND",
        aliases: &["s", "sec", "seconds", "secs"],
    },
    DatePartToken {
        canonical: "MILLISECOND",
        aliases: &[
            "ms",
            "msec",
            "msecs",
            "msecond",
            "mseconds",
            "millisec",
            "millisecs",
            "millisecon",
            "milliseconds",
        ],
    },
    DatePartToken {
        canonical: "MICROSECOND",
        aliases: &[
            "us",
            "usec",
            "usecs",
            "microsec",
            "microsecs",
            "usecond",
            "useconds",
            "microseconds",
        ],
    },
    DatePartToken {
        canonical: "NANOSECOND",
        aliases: &["ns", "nsec", "nanosec", "nsecond", "nseconds", "nanosecs"],
    },
    DatePartToken {
        canonical: "EPOCH",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "EPOCH_SECOND",
        aliases: &["epoch_second", "epoch_seconds"],
    },
    DatePartToken {
        canonical: "EPOCH_MILLISECOND",
        aliases: &["epoch_milliseconds"],
    },
    DatePartToken {
        canonical: "EPOCH_MICROSECOND",
        aliases: &["epoch_microseconds"],
    },
    DatePartToken {
        canonical: "EPOCH_NANOSECOND",
        aliases: &["epoch_nanoseconds"],
    },
    DatePartToken {
        canonical: "TIMEZONE_HOUR",
        aliases: &["tzh"],
    },
    DatePartToken {
        canonical: "TIMEZONE_MINUTE",
        aliases: &["tzm"],
    },
    DatePartToken {
        canonical: "DECADE",
        aliases: &["dec", "decs", "decades"],
    },
    DatePartToken {
        canonical: "MILLENNIUM",
        aliases: &["mil", "mils", "millenia"],
    },
    DatePartToken {
        canonical: "CENTURY",
        aliases: &["c", "cent", "cents", "centuries"],
    },
    DatePartToken {
        canonical: "YEAROFWEEK",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "YEAROFWEEKISO",
        aliases: EMPTY_ALIASES,
    },
];
const MYSQL_PARTS: &[DatePartToken] = &[
    DatePartToken {
        canonical: "MICROSECOND",
        aliases: &["SQL_TSI_MICROSECOND"],
    },
    DatePartToken {
        canonical: "SECOND",
        aliases: &["SQL_TSI_SECOND"],
    },
    DatePartToken {
        canonical: "MINUTE",
        aliases: &["SQL_TSI_MINUTE"],
    },
    DatePartToken {
        canonical: "HOUR",
        aliases: &["SQL_TSI_HOUR"],
    },
    DatePartToken {
        canonical: "DAY",
        aliases: &["SQL_TSI_DAY"],
    },
    DatePartToken {
        canonical: "WEEK",
        aliases: &["SQL_TSI_WEEK"],
    },
    DatePartToken {
        canonical: "MONTH",
        aliases: &["SQL_TSI_MONTH"],
    },
    DatePartToken {
        canonical: "QUARTER",
        aliases: &["SQL_TSI_QUARTER"],
    },
    DatePartToken {
        canonical: "YEAR",
        aliases: &["SQL_TSI_YEAR"],
    },
];
const DATABRICKS_ADD_PARTS: &[DatePartToken] = &[
    DatePartToken {
        canonical: "MICROSECOND",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "MILLISECOND",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "SECOND",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "MINUTE",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "HOUR",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "DAY",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "DAYOFYEAR",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "WEEK",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "MONTH",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "QUARTER",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "YEAR",
        aliases: EMPTY_ALIASES,
    },
];
const DATABRICKS_DIFF_PARTS: &[DatePartToken] = &[
    DatePartToken {
        canonical: "MICROSECOND",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "MILLISECOND",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "SECOND",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "MINUTE",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "HOUR",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "DAY",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "WEEK",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "MONTH",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "QUARTER",
        aliases: EMPTY_ALIASES,
    },
    DatePartToken {
        canonical: "YEAR",
        aliases: EMPTY_ALIASES,
    },
];
const REDSHIFT_COMMON_PARTS: &[DatePartToken] = &[
    DatePartToken {
        canonical: "MILLENNIUM",
        aliases: &["millennia", "mil", "mils"],
    },
    DatePartToken {
        canonical: "CENTURY",
        aliases: &["centuries", "c", "cent", "cents"],
    },
    DatePartToken {
        canonical: "DECADE",
        aliases: &["decades", "dec", "decs"],
    },
    DatePartToken {
        canonical: "YEAR",
        aliases: &["years", "y", "yr", "yrs"],
    },
    DatePartToken {
        canonical: "QUARTER",
        aliases: &["quarters", "qtr", "qtrs"],
    },
    DatePartToken {
        canonical: "MONTH",
        aliases: &["months", "mon", "mons"],
    },
    DatePartToken {
        canonical: "WEEK",
        aliases: &["weeks", "w"],
    },
    DatePartToken {
        canonical: "DAY",
        aliases: &["days", "d"],
    },
    DatePartToken {
        canonical: "HOUR",
        aliases: &["hours", "h", "hr", "hrs"],
    },
    DatePartToken {
        canonical: "MINUTE",
        aliases: &["minutes", "m", "min", "mins"],
    },
    DatePartToken {
        canonical: "SECOND",
        aliases: &["seconds", "s", "sec", "secs"],
    },
    DatePartToken {
        canonical: "MILLISECOND",
        aliases: &[
            "ms",
            "msec",
            "msecs",
            "msecond",
            "mseconds",
            "millisec",
            "millisecs",
            "millisecon",
            "milliseconds",
        ],
    },
    DatePartToken {
        canonical: "MICROSECOND",
        aliases: &[
            "microsec",
            "microsecs",
            "usecond",
            "useconds",
            "us",
            "usec",
            "usecs",
            "microseconds",
        ],
    },
];
const REDSHIFT_DATE_PART_EXTRA: &[DatePartToken] = &[DatePartToken {
    canonical: "DAYOFWEEK",
    aliases: &["dow", "dw", "weekday"],
}];
const MSSQL_COMMON_PARTS: &[DatePartToken] = &[
    DatePartToken {
        canonical: "YEAR",
        aliases: &["yy", "yyyy"],
    },
    DatePartToken {
        canonical: "QUARTER",
        aliases: &["qq", "q"],
    },
    DatePartToken {
        canonical: "MONTH",
        aliases: &["mm", "m"],
    },
    DatePartToken {
        canonical: "DAYOFYEAR",
        aliases: &["dy", "y"],
    },
    DatePartToken {
        canonical: "DAY",
        aliases: &["dd", "d"],
    },
    DatePartToken {
        canonical: "WEEK",
        aliases: &["wk", "ww"],
    },
    DatePartToken {
        canonical: "HOUR",
        aliases: &["hh"],
    },
    DatePartToken {
        canonical: "MINUTE",
        aliases: &["mi", "n"],
    },
    DatePartToken {
        canonical: "SECOND",
        aliases: &["ss", "s"],
    },
    DatePartToken {
        canonical: "MILLISECOND",
        aliases: &["ms"],
    },
    DatePartToken {
        canonical: "MICROSECOND",
        aliases: &["mcs"],
    },
];
const MSSQL_ADD_DIFF_EXTRA: &[DatePartToken] = &[
    DatePartToken {
        canonical: "WEEKDAY",
        aliases: &["dw", "w"],
    },
    DatePartToken {
        canonical: "NANOSECOND",
        aliases: &["ns"],
    },
];
const MSSQL_DATEPART_EXTRA: &[DatePartToken] = &[
    DatePartToken {
        canonical: "WEEKDAY",
        aliases: &["dw"],
    },
    DatePartToken {
        canonical: "NANOSECOND",
        aliases: &["ns"],
    },
    DatePartToken {
        canonical: "TZOFFSET",
        aliases: &["tz"],
    },
    DatePartToken {
        canonical: "ISO_WEEK",
        aliases: &["isowk", "isoww"],
    },
];
const MSSQL_DATETRUNC_EXTRA: &[DatePartToken] = &[DatePartToken {
    canonical: "ISO_WEEK",
    aliases: &["isowk", "isoww"],
}];
const MSSQL_DATE_BUCKET_PARTS: &[DatePartToken] = &[
    DatePartToken {
        canonical: "YEAR",
        aliases: &["yy", "yyyy"],
    },
    DatePartToken {
        canonical: "QUARTER",
        aliases: &["qq", "q"],
    },
    DatePartToken {
        canonical: "MONTH",
        aliases: &["mm", "m"],
    },
    DatePartToken {
        canonical: "DAY",
        aliases: &["dd", "d"],
    },
    DatePartToken {
        canonical: "WEEK",
        aliases: &["wk", "ww"],
    },
    DatePartToken {
        canonical: "HOUR",
        aliases: &["hh"],
    },
    DatePartToken {
        canonical: "MINUTE",
        aliases: &["mi", "n"],
    },
    DatePartToken {
        canonical: "SECOND",
        aliases: &["ss", "s"],
    },
    DatePartToken {
        canonical: "MILLISECOND",
        aliases: &["ms"],
    },
];

static BIGQUERY_PROFILE: DatePartProfile = DatePartProfile {
    token_groups: &[BQ_PARTS],
    allows_weekday_modifier: true,
};
static SNOWFLAKE_PROFILE: DatePartProfile = DatePartProfile {
    token_groups: &[SNOWFLAKE_PARTS],
    allows_weekday_modifier: false,
};
static MYSQL_PROFILE: DatePartProfile = DatePartProfile {
    token_groups: &[MYSQL_PARTS],
    allows_weekday_modifier: false,
};
static DATABRICKS_ADD_PROFILE: DatePartProfile = DatePartProfile {
    token_groups: &[DATABRICKS_ADD_PARTS],
    allows_weekday_modifier: false,
};
static DATABRICKS_DIFF_PROFILE: DatePartProfile = DatePartProfile {
    token_groups: &[DATABRICKS_DIFF_PARTS],
    allows_weekday_modifier: false,
};
static REDSHIFT_ADD_DIFF_PROFILE: DatePartProfile = DatePartProfile {
    token_groups: &[REDSHIFT_COMMON_PARTS],
    allows_weekday_modifier: false,
};
static REDSHIFT_DATE_PART_PROFILE: DatePartProfile = DatePartProfile {
    token_groups: &[REDSHIFT_COMMON_PARTS, REDSHIFT_DATE_PART_EXTRA],
    allows_weekday_modifier: false,
};
static MSSQL_DATEADD_PROFILE: DatePartProfile = DatePartProfile {
    token_groups: &[MSSQL_COMMON_PARTS, MSSQL_ADD_DIFF_EXTRA],
    allows_weekday_modifier: false,
};
static MSSQL_DATEDIFF_PROFILE: DatePartProfile = DatePartProfile {
    token_groups: &[MSSQL_COMMON_PARTS, MSSQL_ADD_DIFF_EXTRA],
    allows_weekday_modifier: false,
};
static MSSQL_DATEPART_PROFILE: DatePartProfile = DatePartProfile {
    token_groups: &[MSSQL_COMMON_PARTS, MSSQL_DATEPART_EXTRA],
    allows_weekday_modifier: false,
};
static MSSQL_DATETRUNC_PROFILE: DatePartProfile = DatePartProfile {
    token_groups: &[MSSQL_COMMON_PARTS, MSSQL_DATETRUNC_EXTRA],
    allows_weekday_modifier: false,
};
static MSSQL_DATE_BUCKET_PROFILE: DatePartProfile = DatePartProfile {
    token_groups: &[MSSQL_DATE_BUCKET_PARTS],
    allows_weekday_modifier: false,
};

const BIGQUERY_DATE_PART: DatePartGrammar = DatePartGrammar {
    profile: &BIGQUERY_PROFILE,
};
const SNOWFLAKE_DATE_PART: DatePartGrammar = DatePartGrammar {
    profile: &SNOWFLAKE_PROFILE,
};
const MYSQL_DATE_PART: DatePartGrammar = DatePartGrammar {
    profile: &MYSQL_PROFILE,
};
const DATABRICKS_DATE_PART: DatePartGrammar = DatePartGrammar {
    profile: &DATABRICKS_ADD_PROFILE,
};
const DATABRICKS_DIFF_DATE_PART: DatePartGrammar = DatePartGrammar {
    profile: &DATABRICKS_DIFF_PROFILE,
};
const REDSHIFT_ADD_DIFF_DATE_PART: DatePartGrammar = DatePartGrammar {
    profile: &REDSHIFT_ADD_DIFF_PROFILE,
};
const REDSHIFT_DATE_PART: DatePartGrammar = DatePartGrammar {
    profile: &REDSHIFT_DATE_PART_PROFILE,
};
const MSSQL_DATEADD_DATE_PART: DatePartGrammar = DatePartGrammar {
    profile: &MSSQL_DATEADD_PROFILE,
};
const MSSQL_DATEDIFF_DATE_PART: DatePartGrammar = DatePartGrammar {
    profile: &MSSQL_DATEDIFF_PROFILE,
};
const MSSQL_DATEPART_DATE_PART: DatePartGrammar = DatePartGrammar {
    profile: &MSSQL_DATEPART_PROFILE,
};
const MSSQL_DATETRUNC_DATE_PART: DatePartGrammar = DatePartGrammar {
    profile: &MSSQL_DATETRUNC_PROFILE,
};
const MSSQL_DATE_BUCKET_DATE_PART: DatePartGrammar = DatePartGrammar {
    profile: &MSSQL_DATE_BUCKET_PROFILE,
};

const BQ_TRUNC_NAMES: &[&str] = &[
    "DATE_TRUNC",
    "DATETIME_TRUNC",
    "TIME_TRUNC",
    "TIMESTAMP_TRUNC",
];
const BQ_TRUNC_WITH_TIMEZONE_NAMES: &[&str] = &["TIMESTAMP_TRUNC"];
const BQ_DIFF_NAMES: &[&str] = &["DATE_DIFF", "DATETIME_DIFF", "TIME_DIFF", "TIMESTAMP_DIFF"];
const LAST_DAY_NAMES: &[&str] = &["LAST_DAY"];
const SNOW_ADD_DIFF_NAMES: &[&str] = &[
    "DATEADD",
    "TIMEADD",
    "TIMESTAMPADD",
    "DATEDIFF",
    "TIMEDIFF",
    "TIMESTAMPDIFF",
];
const SNOW_PART_NAMES: &[&str] = &["DATE_PART", "DATE_TRUNC"];
const SNOW_VALUE_PART_NAMES: &[&str] = &["LAST_DAY", "TRUNC"];
const MYSQL_ADD_DIFF_NAMES: &[&str] = &["TIMESTAMPADD", "TIMESTAMPDIFF"];
const DATABRICKS_ADD_NAMES: &[&str] = &["DATEADD", "DATE_ADD", "TIMESTAMPADD"];
const DATABRICKS_DIFF_NAMES: &[&str] = &["DATEDIFF", "DATE_DIFF", "TIMESTAMPDIFF"];
const REDSHIFT_ADD_DIFF_NAMES: &[&str] = &["DATEADD", "DATEDIFF"];
const REDSHIFT_DATE_PART_NAMES: &[&str] = &["DATE_PART", "PGDATE_PART"];
const MSSQL_DATEADD_NAMES: &[&str] = &["DATEADD"];
const MSSQL_DATEDIFF_NAMES: &[&str] = &["DATEDIFF", "DATEDIFF_BIG"];
const MSSQL_DATEPART_NAMES: &[&str] = &["DATEPART", "DATENAME"];
const MSSQL_DATETRUNC_NAMES: &[&str] = &["DATETRUNC"];
const MSSQL_DATE_BUCKET_NAMES: &[&str] = &["DATE_BUCKET"];

const BIGQUERY_SIGNATURES: &[FunctionSignature] = &[
    FunctionSignature {
        names: BQ_TRUNC_NAMES,
        arity: 2,
        arguments: BQ_VALUE_PART,
    },
    FunctionSignature {
        names: BQ_TRUNC_WITH_TIMEZONE_NAMES,
        arity: 3,
        arguments: BQ_VALUE_PART_VALUE,
    },
    FunctionSignature {
        names: BQ_DIFF_NAMES,
        arity: 3,
        arguments: BQ_VALUE_VALUE_PART,
    },
    FunctionSignature {
        names: LAST_DAY_NAMES,
        arity: 1,
        arguments: BQ_VALUE,
    },
    FunctionSignature {
        names: LAST_DAY_NAMES,
        arity: 2,
        arguments: BQ_VALUE_PART,
    },
];

const SNOWFLAKE_SIGNATURES: &[FunctionSignature] = &[
    FunctionSignature {
        names: SNOW_ADD_DIFF_NAMES,
        arity: 3,
        arguments: SNOW_PART_VALUE_VALUE,
    },
    FunctionSignature {
        names: SNOW_PART_NAMES,
        arity: 2,
        arguments: SNOW_PART_VALUE,
    },
    FunctionSignature {
        names: SNOW_VALUE_PART_NAMES,
        arity: 2,
        arguments: SNOW_VALUE_PART,
    },
];

const MYSQL_SIGNATURES: &[FunctionSignature] = &[FunctionSignature {
    names: MYSQL_ADD_DIFF_NAMES,
    arity: 3,
    arguments: MYSQL_PART_VALUE_VALUE,
}];

const DATABRICKS_SIGNATURES: &[FunctionSignature] = &[
    FunctionSignature {
        names: DATABRICKS_ADD_NAMES,
        arity: 3,
        arguments: DB_PART_VALUE_VALUE,
    },
    FunctionSignature {
        names: DATABRICKS_DIFF_NAMES,
        arity: 3,
        arguments: DB_DIFF_PART_VALUE_VALUE,
    },
];
const REDSHIFT_SIGNATURES: &[FunctionSignature] = &[
    FunctionSignature {
        names: REDSHIFT_ADD_DIFF_NAMES,
        arity: 3,
        arguments: REDSHIFT_PART_VALUE_VALUE,
    },
    FunctionSignature {
        names: REDSHIFT_DATE_PART_NAMES,
        arity: 2,
        arguments: REDSHIFT_PART_VALUE,
    },
];
const MSSQL_SIGNATURES: &[FunctionSignature] = &[
    FunctionSignature {
        names: MSSQL_DATEADD_NAMES,
        arity: 3,
        arguments: MSSQL_PART_VALUE_VALUE,
    },
    FunctionSignature {
        names: MSSQL_DATEDIFF_NAMES,
        arity: 3,
        arguments: MSSQL_DATEDIFF_PART_VALUE_VALUE,
    },
    FunctionSignature {
        names: MSSQL_DATEPART_NAMES,
        arity: 2,
        arguments: MSSQL_PART_VALUE,
    },
    FunctionSignature {
        names: MSSQL_DATETRUNC_NAMES,
        arity: 2,
        arguments: MSSQL_DATETRUNC_PART_VALUE,
    },
    FunctionSignature {
        names: MSSQL_DATE_BUCKET_NAMES,
        arity: 3,
        arguments: MSSQL_DATE_BUCKET_PART_VALUE_VALUE,
    },
    FunctionSignature {
        names: MSSQL_DATE_BUCKET_NAMES,
        arity: 4,
        arguments: MSSQL_DATE_BUCKET_PART_VALUE_VALUE_VALUE,
    },
];

/// Classify a function only when its name, arity, and argument forms exactly
/// match a supported grammar. Returning `None` is intentional: unknown,
/// qualified, quoted, named, or malformed calls retain the generic behavior
/// of walking every expression argument.
pub(crate) fn classify_function(
    dialect: Dialect,
    function: &Function,
) -> Option<&'static FunctionSignature> {
    let args = match &function.args {
        FunctionArguments::List(list) => &list.args,
        _ => return None,
    };
    if args
        .iter()
        .any(|arg| !matches!(arg, FunctionArg::Unnamed(FunctionArgExpr::Expr(_))))
    {
        return None;
    }

    let name = simple_function_name(function)?;
    signature_for(dialect, name, args.len())
}

pub(crate) fn expression_is_static_date_part(expr: &Expr, grammar: DatePartGrammar) -> bool {
    match expr {
        Expr::Identifier(ident) => {
            ident.quote_style.is_none() && grammar.is_part_name(&ident.value)
        }
        Expr::Function(function) if grammar.allows_weekday_modifier() => {
            let Some(name) = simple_function_name(function) else {
                return false;
            };
            if !name.eq_ignore_ascii_case("WEEK") {
                return false;
            }
            let FunctionArguments::List(args) = &function.args else {
                return false;
            };
            let [FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Identifier(weekday)))] =
                args.args.as_slice()
            else {
                return false;
            };
            weekday.quote_style.is_none() && DatePartGrammar::is_weekday(&weekday.value)
        }
        _ => false,
    }
}

fn simple_function_name(function: &Function) -> Option<&str> {
    let [ObjectNamePart::Identifier(ident)] = function.name.0.as_slice() else {
        return None;
    };
    ident.quote_style.is_none().then_some(ident.value.as_str())
}

fn signature_for(dialect: Dialect, name: &str, arity: usize) -> Option<&'static FunctionSignature> {
    profile(dialect).iter().find(|signature| {
        signature.arity == arity
            && signature
                .names
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(name))
    })
}

fn profile(dialect: Dialect) -> &'static [FunctionSignature] {
    match dialect {
        Dialect::BigQuery => BIGQUERY_SIGNATURES,
        Dialect::Snowflake => SNOWFLAKE_SIGNATURES,
        Dialect::MySql => MYSQL_SIGNATURES,
        Dialect::Databricks => DATABRICKS_SIGNATURES,
        Dialect::Redshift => REDSHIFT_SIGNATURES,
        Dialect::MsSql => MSSQL_SIGNATURES,
        Dialect::Generic
        | Dialect::Ansi
        | Dialect::PostgreSql
        | Dialect::Hive
        | Dialect::DuckDb
        | Dialect::Trino
        | Dialect::Spark
        | Dialect::ClickHouse
        | Dialect::SQLite => &[],
    }
}

impl DatePartGrammar {
    fn allows_weekday_modifier(self) -> bool {
        self.profile.allows_weekday_modifier
    }

    fn is_part_name(self, value: &str) -> bool {
        self.profile.token_groups.iter().any(|group| {
            group.iter().any(|token| {
                token.canonical.eq_ignore_ascii_case(value)
                    || token
                        .aliases
                        .iter()
                        .any(|alias| alias.eq_ignore_ascii_case(value))
            })
        })
    }

    fn is_weekday(value: &str) -> bool {
        matches!(
            value.to_ascii_uppercase().as_str(),
            "SUNDAY" | "MONDAY" | "TUESDAY" | "WEDNESDAY" | "THURSDAY" | "FRIDAY" | "SATURDAY"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Dialect;
    use sqlparser::ast::{FunctionArg, FunctionArgOperator, ObjectNamePart, Statement};
    use sqlparser::dialect::BigQueryDialect;
    use sqlparser::parser::Parser;

    fn function(sql: &str) -> Function {
        let statement = Parser::parse_sql(&BigQueryDialect, sql)
            .expect("SQL should parse")
            .remove(0);
        let Statement::Query(query) = statement else {
            panic!("expected query")
        };
        let sqlparser::ast::SetExpr::Select(select) = *query.body else {
            panic!("expected SELECT")
        };
        let sqlparser::ast::SelectItem::UnnamedExpr(Expr::Function(function)) =
            select.projection.into_iter().next().unwrap()
        else {
            panic!("expected function")
        };
        function
    }

    #[test]
    fn bigquery_signatures_classify_exact_roles() {
        let function = function("SELECT DATE_DIFF(a, b, ISOWEEK) FROM t");
        let signature = classify_function(Dialect::BigQuery, &function).unwrap();
        let roles = signature.arguments;
        assert_eq!(signature.arity, 3);
        assert_eq!(roles[0], ArgumentSemantic::ValueExpression);
        assert!(matches!(roles[2], ArgumentSemantic::DatePart(_)));
    }

    #[test]
    fn profile_table_covers_representative_signatures() {
        let cases = [
            (Dialect::BigQuery, "SELECT DATE_DIFF(a, b, DAY) FROM t"),
            (Dialect::Snowflake, "SELECT DATEADD(DAY, a, b) FROM t"),
            (Dialect::MySql, "SELECT TIMESTAMPDIFF(DAY, a, b) FROM t"),
            (Dialect::Databricks, "SELECT DATEDIFF(DAY, a, b) FROM t"),
            (Dialect::Redshift, "SELECT DATEADD(DAY, a, b) FROM t"),
            (Dialect::MsSql, "SELECT DATEADD(DAY, a, b) FROM t"),
        ];
        for (dialect, sql) in cases {
            let function = function(sql);
            let signature = classify_function(dialect, &function).unwrap();
            let roles = signature.arguments;
            assert_eq!(signature.arity, roles.len());
            assert!(
                roles
                    .iter()
                    .any(|role| { matches!(role, ArgumentSemantic::DatePart(_)) })
            );
        }
    }

    #[test]
    fn unsupported_dialect_falls_back() {
        let function = function("SELECT DATE_DIFF(a, b, ISOWEEK) FROM t");
        for dialect in [
            Dialect::Generic,
            Dialect::Ansi,
            Dialect::PostgreSql,
            Dialect::Hive,
            Dialect::DuckDb,
            Dialect::Trino,
            Dialect::Spark,
            Dialect::ClickHouse,
            Dialect::SQLite,
        ] {
            assert!(
                classify_function(dialect, &function).is_none(),
                "unexpected temporal profile for {dialect:?}"
            );
        }
    }

    #[test]
    fn qualified_quoted_named_and_wrong_arity_calls_fall_back() {
        let mut qualified = function("SELECT DATE_DIFF(a, b, ISOWEEK) FROM t");
        qualified.name.0.insert(
            0,
            ObjectNamePart::Identifier(sqlparser::ast::Ident::new("project")),
        );
        assert!(classify_function(Dialect::BigQuery, &qualified).is_none());

        let mut quoted = function("SELECT DATE_DIFF(a, b, ISOWEEK) FROM t");
        let ObjectNamePart::Identifier(ident) = &mut quoted.name.0[0] else {
            panic!("expected identifier function name")
        };
        ident.quote_style = Some('"');
        assert!(classify_function(Dialect::BigQuery, &quoted).is_none());

        let mut named = function("SELECT DATE_DIFF(a, b, ISOWEEK) FROM t");
        let FunctionArguments::List(args) = &mut named.args else {
            panic!("expected argument list")
        };
        let FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) = args.args[0].clone() else {
            panic!("expected expression argument")
        };
        args.args[0] = FunctionArg::Named {
            name: sqlparser::ast::Ident::new("date"),
            arg: FunctionArgExpr::Expr(expr),
            operator: FunctionArgOperator::Equals,
        };
        assert!(classify_function(Dialect::BigQuery, &named).is_none());

        let mut wrong_arity = function("SELECT DATE_DIFF(a, b, ISOWEEK) FROM t");
        let FunctionArguments::List(args) = &mut wrong_arity.args else {
            panic!("expected argument list")
        };
        args.args.pop();
        assert!(classify_function(Dialect::BigQuery, &wrong_arity).is_none());
    }

    #[test]
    fn static_date_part_requires_known_grammar() {
        assert!(expression_is_static_date_part(
            &Expr::Identifier(sqlparser::ast::Ident::new("ISOYEAR")),
            BIGQUERY_DATE_PART
        ));
        assert!(!expression_is_static_date_part(
            &Expr::Identifier(sqlparser::ast::Ident::new("made_up_part")),
            BIGQUERY_DATE_PART
        ));
    }

    #[test]
    fn date_part_alias_tables_are_profile_scoped() {
        let aliases = [
            (SNOWFLAKE_DATE_PART, "yyyy"),
            (SNOWFLAKE_DATE_PART, "yyy"),
            (SNOWFLAKE_DATE_PART, "years"),
            (SNOWFLAKE_DATE_PART, "mon"),
            (SNOWFLAKE_DATE_PART, "months"),
            (SNOWFLAKE_DATE_PART, "dayofmonth"),
            (SNOWFLAKE_DATE_PART, "dow"),
            (SNOWFLAKE_DATE_PART, "dayofweek_iso"),
            (SNOWFLAKE_DATE_PART, "wk"),
            (SNOWFLAKE_DATE_PART, "weekofyeariso"),
            (SNOWFLAKE_DATE_PART, "qtr"),
            (SNOWFLAKE_DATE_PART, "quarters"),
            (SNOWFLAKE_DATE_PART, "hh"),
            (SNOWFLAKE_DATE_PART, "hours"),
            (SNOWFLAKE_DATE_PART, "min"),
            (SNOWFLAKE_DATE_PART, "minutes"),
            (SNOWFLAKE_DATE_PART, "us"),
            (SNOWFLAKE_DATE_PART, "microseconds"),
            (SNOWFLAKE_DATE_PART, "epoch_milliseconds"),
            (SNOWFLAKE_DATE_PART, "epoch_second"),
            (SNOWFLAKE_DATE_PART, "yearofweek"),
            (SNOWFLAKE_DATE_PART, "yearofweekiso"),
            (MYSQL_DATE_PART, "SQL_TSI_DAY"),
            (DATABRICKS_DATE_PART, "DAYOFYEAR"),
            (REDSHIFT_ADD_DIFF_DATE_PART, "m"),
            (REDSHIFT_ADD_DIFF_DATE_PART, "w"),
            (REDSHIFT_ADD_DIFF_DATE_PART, "mon"),
            (MSSQL_DATEPART_DATE_PART, "tz"),
            (MSSQL_DATEPART_DATE_PART, "isoww"),
        ];
        for (grammar, alias) in aliases {
            assert!(
                grammar.is_part_name(alias),
                "alias {alias} was not recognized"
            );
        }
        assert!(!SNOWFLAKE_DATE_PART.is_part_name("fortnight"));
        assert!(!MYSQL_DATE_PART.is_part_name("yyyy"));
        assert!(!MYSQL_DATE_PART.is_part_name("DAY_SECOND"));
        assert!(!DATABRICKS_DATE_PART.is_part_name("DAYOFWEEK"));
        assert!(!DATABRICKS_DATE_PART.is_part_name("WEEKOFYEAR"));
        assert!(!REDSHIFT_ADD_DIFF_DATE_PART.is_part_name("mm"));
        assert!(!REDSHIFT_ADD_DIFF_DATE_PART.is_part_name("wk"));
        assert!(!REDSHIFT_ADD_DIFF_DATE_PART.is_part_name("DOW"));
        assert!(REDSHIFT_DATE_PART.is_part_name("DOW"));
        assert!(!REDSHIFT_DATE_PART.is_part_name("DAYOFYEAR"));
        assert!(!MSSQL_DATE_BUCKET_DATE_PART.is_part_name("mcs"));
        assert!(!MSSQL_DATETRUNC_DATE_PART.is_part_name("NANOSECOND"));
    }

    #[test]
    fn overloads_and_profile_specific_date_parts_are_conservative() {
        let bq_timezone = function("SELECT DATE_TRUNC(value, DAY, timezone) FROM t");
        assert!(classify_function(Dialect::BigQuery, &bq_timezone).is_none());

        let mysql_composite = function("SELECT TIMESTAMPDIFF(DAY_SECOND, a, b) FROM t");
        assert!(classify_function(Dialect::MySql, &mysql_composite).is_some());
        let mysql_signature = classify_function(Dialect::MySql, &mysql_composite).unwrap();
        assert!(!expression_is_static_date_part(
            &Expr::Identifier(sqlparser::ast::Ident::new("DAY_SECOND")),
            match mysql_signature.arguments[0] {
                ArgumentSemantic::DatePart(grammar) => grammar,
                ArgumentSemantic::ValueExpression => panic!("expected date-part role"),
            }
        ));

        let databricks_add = function("SELECT DATEADD(DAYOFYEAR, amount, ts) FROM t");
        let add_signature = classify_function(Dialect::Databricks, &databricks_add).unwrap();
        assert!(matches!(
            add_signature.arguments[0],
            ArgumentSemantic::DatePart(_)
        ));
        let databricks_diff = function("SELECT DATEDIFF(DAYOFYEAR, start_ts, end_ts) FROM t");
        let diff_signature = classify_function(Dialect::Databricks, &databricks_diff).unwrap();
        assert!(matches!(
            diff_signature.arguments[0],
            ArgumentSemantic::DatePart(_)
        ));
        let ArgumentSemantic::DatePart(diff_grammar) = diff_signature.arguments[0] else {
            unreachable!()
        };
        assert!(!expression_is_static_date_part(
            &Expr::Identifier(sqlparser::ast::Ident::new("DAYOFYEAR")),
            diff_grammar
        ));
    }

    #[test]
    fn redshift_and_mssql_profiles_keep_family_specific_static_parts() {
        let redshift_part = function("SELECT DATE_PART(DOW, event_ts) FROM t");
        let redshift_signature =
            classify_function(Dialect::Redshift, &redshift_part).expect("Redshift DATE_PART");
        let ArgumentSemantic::DatePart(redshift_grammar) = redshift_signature.arguments[0] else {
            panic!("expected Redshift date-part role")
        };
        assert!(expression_is_static_date_part(
            &Expr::Identifier(sqlparser::ast::Ident::new("DOW")),
            redshift_grammar
        ));
        assert!(
            classify_function(
                Dialect::Redshift,
                &function("SELECT DATE_TRUNC(event_ts, WEEK) FROM t")
            )
            .is_none()
        );

        let mssql_trunc = function("SELECT DATETRUNC(WEEK, event_ts) FROM t");
        let mssql_signature =
            classify_function(Dialect::MsSql, &mssql_trunc).expect("MsSql DATETRUNC");
        let ArgumentSemantic::DatePart(mssql_grammar) = mssql_signature.arguments[0] else {
            panic!("expected MsSql date-part role")
        };
        assert!(expression_is_static_date_part(
            &Expr::Identifier(sqlparser::ast::Ident::new("WEEK")),
            mssql_grammar
        ));
        for unsupported in ["WEEKDAY", "TZOFFSET", "NANOSECOND"] {
            assert!(!expression_is_static_date_part(
                &Expr::Identifier(sqlparser::ast::Ident::new(unsupported)),
                mssql_grammar
            ));
        }
        let mssql_dateadd = function("SELECT DATEADD(TZOFFSET, amount, event_ts) FROM t");
        let ArgumentSemantic::DatePart(mssql_dateadd_grammar) =
            classify_function(Dialect::MsSql, &mssql_dateadd)
                .expect("MsSql DATEADD")
                .arguments[0]
        else {
            panic!("expected MsSql DATEADD date-part role")
        };
        assert!(!expression_is_static_date_part(
            &Expr::Identifier(sqlparser::ast::Ident::new("TZOFFSET")),
            mssql_dateadd_grammar
        ));
        assert!(
            classify_function(
                Dialect::Spark,
                &function("SELECT DATEDIFF(DAY, a, b) FROM t")
            )
            .is_none()
        );
    }
}
