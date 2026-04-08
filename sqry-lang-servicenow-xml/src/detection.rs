//! `ServiceNow` XML record type detection and fast pre-check.

/// Maximum bytes to scan for the `record_update` marker.
const PRECHECK_WINDOW: usize = 512;

/// Fast pre-check: scan first 512 bytes for "`record_update`" string.
///
/// Eliminates 99%+ of non-ServiceNow XML files in <1μs without
/// an expensive roxmltree parse.
#[must_use]
pub fn fast_precheck(content: &[u8]) -> bool {
    let window = &content[..content.len().min(PRECHECK_WINDOW)];
    window
        .windows(b"record_update".len())
        .any(|w| w == b"record_update")
}

/// `ServiceNow` record type classification.
#[derive(Debug, Clone)]
pub enum RecordType {
    /// Script-bearing record (Business Rule, Script Include, etc.).
    /// Contains the list of XML element names that hold script content.
    Script(Vec<&'static str>),
    /// Table schema record (`sys_dictionary`).
    TableSchema,
    /// Table definition record (`sys_db_object`).
    TableDefinition,
}

impl RecordType {
    /// Map a `ServiceNow` table name to its record type.
    #[must_use]
    pub fn from_table(table: &str) -> Option<Self> {
        match table {
            "sys_script" | "sys_script_include" | "sys_script_client" => {
                Some(Self::Script(vec!["script"]))
            }
            "sys_ui_action" => Some(Self::Script(vec!["script", "client_script"])),
            "sys_ui_policy" => Some(Self::Script(vec!["script_true", "script_false"])),
            "sys_ws_operation" => Some(Self::Script(vec!["operation_script"])),
            "sys_processor" => Some(Self::Script(vec!["script"])),
            "sys_dictionary" => Some(Self::TableSchema),
            "sys_db_object" => Some(Self::TableDefinition),
            _ => None,
        }
    }

    /// Returns true if this is a script-bearing record type.
    #[must_use]
    pub fn is_script(&self) -> bool {
        matches!(self, Self::Script(_))
    }

    /// Returns true if this is a table schema record.
    #[must_use]
    pub fn is_table_schema(&self) -> bool {
        matches!(self, Self::TableSchema)
    }

    /// Returns true if this is a table definition record.
    #[must_use]
    pub fn is_table_definition(&self) -> bool {
        matches!(self, Self::TableDefinition)
    }

    /// Get the script field names for script-bearing records.
    #[must_use]
    pub fn script_fields(&self) -> Option<&[&'static str]> {
        match self {
            Self::Script(fields) => Some(fields),
            Self::TableSchema | Self::TableDefinition => None,
        }
    }
}
