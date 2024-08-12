use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A set of grants.
#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
pub struct Grants {
    /// An optional identifier.
    pub name: Option<String>,
    pub user: String,
    pub database: Option<DatabaseGrant>,
    pub schema: Option<SchemaGrant>,
    pub table: Option<TableGrant>,
}

/// A set of database-specific grants.
#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
pub struct DatabaseGrant {
    /// The granted privileges.
    pub grants: Vec<DatabasePrivilege>,
}

/// A set of table-specific grants.
#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
pub struct SchemaGrant {
    pub name: String,
    /// The granted privileges.
    pub grants: Vec<SchemaPrivilege>,
}

/// A set of table-specific grants.
#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
pub struct TableGrant {
    /// The table name.
    pub name: String,
    /// The schema name.
    pub schema: Option<String>,
    /// The granted privileges.
    pub grants: Vec<TablePrivilege>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub enum DatabasePrivilege {
    #[serde(rename = "CONNECT")]
    Connect,
    #[serde(rename = "CREATE")]
    Create,
    #[serde(rename = "TEMPORARY")]
    Temporary,
    #[serde(rename = "ALL PRIVILEGES")]
    AllPrivileges,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub enum SchemaPrivilege {
    #[serde(rename = "USAGE")]
    Usage,
    #[serde(rename = "CREATE")]
    Create,
    #[serde(rename = "ALL PRIVILEGES")]
    AllPrivileges,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub enum TablePrivilege {
    #[serde(rename = "SELECT")]
    Select,
    #[serde(rename = "INSERT")]
    Insert,
    #[serde(rename = "UPDATE")]
    Update,
    #[serde(rename = "DELETE")]
    Delete,
    #[serde(rename = "TRUNCATE")]
    Truncate,
    #[serde(rename = "REFERENCES")]
    References,
    #[serde(rename = "TRIGGER")]
    Trigger,
    #[serde(rename = "ALL PRIVILEGES")]
    AllPrivileges,
}

pub trait ToSql {
    fn to_sql(&self) -> &str;
}

impl ToSql for DatabasePrivilege {
    fn to_sql(&self) -> &str {
        match self {
            DatabasePrivilege::Connect => "CONNECT",
            DatabasePrivilege::Create => "CREATE",
            DatabasePrivilege::Temporary => "TEMPORARY",
            DatabasePrivilege::AllPrivileges => "ALL PRIVILEGES",
        }
    }
}

impl ToSql for SchemaPrivilege {
    fn to_sql(&self) -> &str {
        match self {
            SchemaPrivilege::Usage => "USAGE",
            SchemaPrivilege::Create => "CREATE",
            SchemaPrivilege::AllPrivileges => "ALL PRIVILEGES",
        }
    }
}

impl ToSql for TablePrivilege {
    fn to_sql(&self) -> &str {
        match self {
            TablePrivilege::Select => "SELECT",
            TablePrivilege::Insert => "INSERT",
            TablePrivilege::Update => "UPDATE",
            TablePrivilege::Delete => "DELETE",
            TablePrivilege::Truncate => "TRUNCATE",
            TablePrivilege::References => "REFERENCES",
            TablePrivilege::Trigger => "TRIGGER",
            TablePrivilege::AllPrivileges => "ALL PRIVILEGES",
        }
    }
}
