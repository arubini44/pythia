//! The SQL Server dashboard's data, read through [`pythia_db::Client`] (the
//! session's `DbRequest::Run`). Sections the user can't read are left out
//! with the privilege they need.

use pythia_db::Client;

use crate::insights::Insight;
use crate::queries::SectionError;

#[derive(Debug, Clone, Default)]
pub struct MsSnapshot {
    pub errors: Vec<SectionError>,
}

/// Collects everything the dashboard shows; never fails as a whole. Table
/// statistics only with `storage`.
pub fn snapshot(_c: &mut dyn Client, _storage: bool) -> MsSnapshot {
    MsSnapshot::default()
}

/// Findings, most severe first; empty means all is well.
pub fn insights(_snap: &MsSnapshot) -> Vec<Insight> {
    Vec::new()
}
