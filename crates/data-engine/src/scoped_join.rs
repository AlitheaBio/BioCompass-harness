//! A read-only, single-key aggregate over a provider-approved relation.

use super::{
    CompassError, CompassResult, DatasetGrant, DuckLakeCatalogEngine, FilterExpr, GrantLevel,
    ScalarValue, SealedSnapshot, SealedTableSnapshot, build_sealed_table_scan_sql,
    find_schema_column, quote_ident, validate_sql_ident,
};
use biocompass_core_traits::ScopedQueryDefinition;
use duckdb::{Row, params_from_iter, types::Value as DuckValue};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

/// The trusted publisher supplies this relation when sealing a release.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeRelation {
    pub parent_table: String,
    pub parent_key: String,
    pub child_table: String,
    pub child_foreign_key: String,
    /// Membership lookups that may filter the scoped child rows without exporting the linked data.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub paths: BTreeMap<String, MembershipPath>,
}

/// The bridge associates values on a fact row with a signed-integer or text dimension ID.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MembershipPath {
    pub child_key: String,
    pub bridge_table: String,
    pub bridge_child_key: String,
    pub bridge_dimension_key: String,
    pub dimension_table: String,
    pub dimension_key: String,
}

/// A module may choose the signed integer measure on the approved child relation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScopedJoinSum {
    pub measure: String,
}

/// Text and interoperable JSON integer keys have distinct wire types.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ScopeKey {
    Int(i64),
    Text(String),
}

impl ScopeKey {
    pub fn is_wire_safe(&self) -> bool {
        match self {
            // A browser can round a JSON number before sending it to the provider.
            Self::Int(id) => id.unsigned_abs() <= 9_007_199_254_740_991,
            Self::Text(id) => !id.is_empty() && id.len() <= 256,
        }
    }

    pub fn from_scalar(value: &ScalarValue) -> Option<Self> {
        match value {
            ScalarValue::Int(id) => Some(Self::Int(*id)),
            ScalarValue::Text(id) => Some(Self::Text(id.clone())),
            _ => None,
        }
    }
}

impl From<&ScopeKey> for ScalarValue {
    fn from(key: &ScopeKey) -> Self {
        match key {
            ScopeKey::Int(id) => Self::Int(*id),
            ScopeKey::Text(id) => Self::Text(id.clone()),
        }
    }
}

impl From<&str> for ScopeKey {
    fn from(key: &str) -> Self {
        Self::Text(key.to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeKeyParameter {
    pub id: ScopeKey,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MembershipParameter {
    id: ScopeKey,
    related_id: ScopeKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScopedGroupSum {
    pub id: ScopeKey,
    pub count: u64,
    pub sum: Option<i64>,
}

fn invalid(message: &str) -> CompassError {
    CompassError::Validation(message.to_owned())
}

fn query_error(error: duckdb::Error) -> CompassError {
    CompassError::Storage(format!("scoped join aggregate: {error}"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum KeyType {
    Text,
    TinyInt,
    SmallInt,
    Int,
    BigInt,
}

impl KeyType {
    fn from_column(data_type: &str) -> Option<Self> {
        match data_type.to_ascii_uppercase().as_str() {
            "VARCHAR" => Some(Self::Text),
            "TINYINT" => Some(Self::TinyInt),
            "SMALLINT" => Some(Self::SmallInt),
            "INTEGER" => Some(Self::Int),
            "BIGINT" => Some(Self::BigInt),
            _ => None,
        }
    }

    fn bind(self, key: &ScopeKey) -> Option<DuckValue> {
        match (self, key) {
            (Self::Text, ScopeKey::Text(id)) => Some(DuckValue::Text(id.clone())),
            (Self::TinyInt, ScopeKey::Int(id)) => i8::try_from(*id).ok().map(DuckValue::TinyInt),
            (Self::SmallInt, ScopeKey::Int(id)) => i16::try_from(*id).ok().map(DuckValue::SmallInt),
            (Self::Int, ScopeKey::Int(id)) => i32::try_from(*id).ok().map(DuckValue::Int),
            (Self::BigInt, ScopeKey::Int(id)) => Some(DuckValue::BigInt(*id)),
            _ => None,
        }
    }

    fn read(self, row: &Row<'_>) -> duckdb::Result<ScopeKey> {
        if self == Self::Text {
            row.get(0).map(ScopeKey::Text)
        } else {
            row.get(0).map(ScopeKey::Int)
        }
    }
}

impl ScopeRelation {
    /// Check the pinned snapshot, including the primary key declared by the publisher.
    pub fn validate(&self, snapshot: &SealedSnapshot) -> CompassResult<()> {
        let (_, child, _) = self.resolve(snapshot)?;
        if self.paths.len() > 16 {
            return Err(invalid(
                "a scope relation supports at most 16 membership paths",
            ));
        }
        for (name, path) in &self.paths {
            validate_sql_ident(name)?;
            path.resolve(self, child, snapshot)?;
        }
        Ok(())
    }

    /// Validate the module's choice against the relationship names and schema sealed by the provider.
    pub fn validate_query(
        &self,
        query: &ScopedQueryDefinition,
        snapshot: &SealedSnapshot,
    ) -> CompassResult<()> {
        let (_, child, _) = self.resolve(snapshot)?;
        if let Some(path) = query.path() {
            self.membership(path, child, snapshot)?;
        }
        validate_measure(query.measure(), child)
    }

    fn membership<'a>(
        &'a self,
        name: &str,
        child: &SealedTableSnapshot,
        snapshot: &'a SealedSnapshot,
    ) -> CompassResult<ResolvedMembership<'a>> {
        validate_sql_ident(name)?;
        let path = self
            .paths
            .get(name)
            .ok_or_else(|| invalid("module selected an unapproved membership path"))?;
        path.resolve(self, child, snapshot)
    }

    pub fn execute_query(
        &self,
        query: &ScopedQueryDefinition,
        engine: &DuckLakeCatalogEngine,
        snapshot_id: u64,
        digest: &str,
        grant: &DatasetGrant,
        params: serde_json::Value,
    ) -> CompassResult<Option<ScopedGroupSum>> {
        let bad_params = |_| invalid("invalid typed scoped query parameters");
        let (id, filter) = if let Some(path) = query.path() {
            let p: MembershipParameter = serde_json::from_value(params).map_err(bad_params)?;
            (p.id, Some((path, p.related_id)))
        } else {
            let p: ScopeKeyParameter = serde_json::from_value(params).map_err(bad_params)?;
            (p.id, None)
        };
        self.aggregate(
            query.measure(),
            engine,
            snapshot_id,
            digest,
            grant,
            &id,
            filter,
        )
    }

    fn resolve<'a>(
        &self,
        snapshot: &'a SealedSnapshot,
    ) -> CompassResult<(&'a SealedTableSnapshot, &'a SealedTableSnapshot, KeyType)> {
        for name in [
            &self.parent_table,
            &self.parent_key,
            &self.child_table,
            &self.child_foreign_key,
        ] {
            validate_sql_ident(name)?;
        }
        if self.parent_table == self.child_table {
            return Err(invalid("scope relation must link two different tables"));
        }
        let parent = snapshot.resolve_table(Some(&self.parent_table))?;
        let child = snapshot.resolve_table(Some(&self.child_table))?;
        let parent_key = find_schema_column(&self.parent_key, &parent.schema)?;
        let foreign_key = find_schema_column(&self.child_foreign_key, &child.schema)?;
        let key_type = KeyType::from_column(&parent_key.data_type)
            .filter(|kind| Some(*kind) == KeyType::from_column(&foreign_key.data_type))
            .filter(|_| parent.schema.primary_key.as_slice() == [self.parent_key.as_str()])
            .ok_or_else(|| invalid(
                "scope relation requires a primary key and foreign key of the same text or signed integer type",
            ))?;
        Ok((parent, child, key_type))
    }

    /// A provider grant binds a bounded, typed set on exactly the approved anchor column.
    pub fn scoped_ids(
        &self,
        grant: &DatasetGrant,
        snapshot: &SealedSnapshot,
    ) -> CompassResult<HashSet<ScopeKey>> {
        let (_, _, key_type) = self.resolve(snapshot)?;
        self.scoped_ids_for_type(grant, key_type)
    }

    /// Validate a proposed key list against this published release.
    pub fn validate_keys(&self, snapshot: &SealedSnapshot, keys: &[ScopeKey]) -> CompassResult<()> {
        let (_, _, key_type) = self.resolve(snapshot)?;
        if keys
            .iter()
            .all(|key| key.is_wire_safe() && key_type.bind(key).is_some())
        {
            Ok(())
        } else {
            Err(invalid(
                "grant keys must be exact and match the declared parent key type",
            ))
        }
    }

    fn scoped_ids_for_type(
        &self,
        grant: &DatasetGrant,
        key_type: KeyType,
    ) -> CompassResult<HashSet<ScopeKey>> {
        let denied = || {
            CompassError::AccessDenied(
                "scope relation requires a bounded grant matching its declared parent key and type"
                    .into(),
            )
        };
        let Some(FilterExpr::In { column, values }) = &grant.row_scope else {
            return Err(denied());
        };
        if column != &self.parent_key || values.len() > 10_000 {
            return Err(denied());
        }
        values
            .iter()
            .map(|value| {
                ScopeKey::from_scalar(value)
                    .filter(ScopeKey::is_wire_safe)
                    .filter(|key| key_type.bind(key).is_some())
                    .ok_or_else(denied)
            })
            .collect()
    }
}

struct ResolvedMembership<'a> {
    path: &'a MembershipPath,
    bridge: &'a SealedTableSnapshot,
    dimension: &'a SealedTableSnapshot,
    key_type: KeyType,
}

impl MembershipPath {
    fn resolve<'a>(
        &'a self,
        anchor: &ScopeRelation,
        child: &SealedTableSnapshot,
        snapshot: &'a SealedSnapshot,
    ) -> CompassResult<ResolvedMembership<'a>> {
        for name in [
            &self.child_key,
            &self.bridge_table,
            &self.bridge_child_key,
            &self.bridge_dimension_key,
            &self.dimension_table,
            &self.dimension_key,
        ] {
            validate_sql_ident(name)?;
        }
        let tables = [
            &anchor.parent_table,
            &anchor.child_table,
            &self.bridge_table,
            &self.dimension_table,
        ];
        if tables.into_iter().collect::<HashSet<_>>().len() != 4
            || self.bridge_child_key == self.bridge_dimension_key
        {
            return Err(invalid(
                "membership requires four distinct tables and two bridge columns",
            ));
        }
        if child.schema.primary_key.is_empty() {
            return Err(invalid(
                "membership aggregate requires a declared measurement primary key",
            ));
        }
        for column in &child.schema.primary_key {
            find_schema_column(column, &child.schema)?;
        }
        let bridge = snapshot.resolve_table(Some(&self.bridge_table))?;
        let dimension = snapshot.resolve_table(Some(&self.dimension_table))?;
        if dimension.schema.primary_key.as_slice() != [self.dimension_key.as_str()] {
            return Err(invalid(
                "membership dimension must have the declared single-column primary key",
            ));
        }
        let matching_keys = |left: &SealedTableSnapshot,
                             lk: &str,
                             right: &SealedTableSnapshot,
                             rk: &str| {
            let lt = find_schema_column(lk, &left.schema)?;
            let rt = find_schema_column(rk, &right.schema)?;
            KeyType::from_column(&lt.data_type)
                .filter(|t| Some(*t) == KeyType::from_column(&rt.data_type))
                .ok_or_else(|| {
                    invalid("membership links require matching text or signed integer column types")
                })
        };
        matching_keys(child, &self.child_key, bridge, &self.bridge_child_key)?;
        let key_type = matching_keys(
            dimension,
            &self.dimension_key,
            bridge,
            &self.bridge_dimension_key,
        )?;
        Ok(ResolvedMembership {
            path: self,
            bridge,
            dimension,
            key_type,
        })
    }
}

impl ResolvedMembership<'_> {
    fn predicate(&self, engine: &DuckLakeCatalogEngine) -> CompassResult<String> {
        let bridge = build_sealed_table_scan_sql(self.bridge, engine.shard_cache())?;
        let dimension = build_sealed_table_scan_sql(self.dimension, engine.shard_cache())?;
        let child_link = quote_ident(&self.path.child_key)?;
        let bridge_link = quote_ident(&self.path.bridge_child_key)?;
        let bridge_dimension = quote_ident(&self.path.bridge_dimension_key)?;
        let dimension_id = quote_ident(&self.path.dimension_key)?;
        // A repeated bridge association qualifies a measurement once. The dimension lookup has
        // no output of its own, including when its key exists solely outside this run's scope.
        Ok(format!(
            " AND EXISTS (SELECT 1 FROM ({bridge}) b
            JOIN ({dimension}) d ON d.{dimension_id} = b.{bridge_dimension}
            WHERE b.{bridge_link} = fact.{child_link} AND d.{dimension_id} = ?)"
        ))
    }
}

fn validate_measure(name: &str, child: &SealedTableSnapshot) -> CompassResult<()> {
    let measure = find_schema_column(name, &child.schema)?;
    if name != measure.name
        || !matches!(
            measure.data_type.as_str(),
            "TINYINT" | "SMALLINT" | "INTEGER" | "BIGINT"
        )
    {
        return Err(invalid(
            "sum requires a signed integer measure on the child table",
        ));
    }
    Ok(())
}

impl ScopedJoinSum {
    pub fn validate(
        &self,
        relation: &ScopeRelation,
        snapshot: &SealedSnapshot,
    ) -> CompassResult<()> {
        let (_, child, _) = relation.resolve(snapshot)?;
        validate_measure(&self.measure, child)
    }

    /// Execute with the original grant and release pin. The provider rechecks active authorization.
    pub fn execute(
        &self,
        relation: &ScopeRelation,
        engine: &DuckLakeCatalogEngine,
        snapshot_id: u64,
        dataset_digest: &str,
        grant: &DatasetGrant,
        parameter: &ScopeKeyParameter,
    ) -> CompassResult<Option<ScopedGroupSum>> {
        relation.aggregate(
            &self.measure,
            engine,
            snapshot_id,
            dataset_digest,
            grant,
            &parameter.id,
            None,
        )
    }
}

impl ScopeRelation {
    fn aggregate(
        &self,
        measure: &str,
        engine: &DuckLakeCatalogEngine,
        snapshot_id: u64,
        dataset_digest: &str,
        grant: &DatasetGrant,
        id: &ScopeKey,
        filter: Option<(&str, ScopeKey)>,
    ) -> CompassResult<Option<ScopedGroupSum>> {
        if grant.level != GrantLevel::ComputeOnly || grant.catalog_id != engine.summary().id {
            return Err(CompassError::AccessDenied(
                "scoped join aggregate requires compute access to this catalog".into(),
            ));
        }
        let snapshot = engine.load_sealed_snapshot(Some(snapshot_id))?;
        if snapshot.dataset_digest != dataset_digest {
            return Err(CompassError::IntegrityViolation(
                "scoped join aggregate dataset digest mismatch".into(),
            ));
        }
        let (parent, child, key_type) = self.resolve(&snapshot)?;
        validate_measure(measure, child)?;
        let membership = if let Some((name, key)) = filter {
            let path = self.membership(name, child, &snapshot)?;
            let bound = path
                .key_type
                .bind(&key)
                .filter(|_| key.is_wire_safe())
                .ok_or_else(|| {
                    invalid("related_id must exactly match the declared dimension primary-key type")
                })?;
            Some((path, bound))
        } else {
            None
        };
        let allowed_ids = self.scoped_ids_for_type(grant, key_type)?;
        let key = id.is_wire_safe().then(|| key_type.bind(id))
            .flatten()
            .ok_or_else(|| invalid(
                "scope key must match the declared type and be text of 1 to 256 bytes or an exact JSON integer",
            ))?;
        if !allowed_ids.contains(id) {
            return Ok(None);
        }

        let parent_key = quote_ident(&self.parent_key)?;
        let child_key = quote_ident(&self.child_foreign_key)?;
        let measure = quote_ident(measure)?;
        let parent_scan = build_sealed_table_scan_sql(parent, engine.shard_cache())?;
        let child_scan = build_sealed_table_scan_sql(child, engine.shard_cache())?;
        let predicate = membership
            .as_ref()
            .map(|(path, _)| path.predicate(engine))
            .transpose()?
            .unwrap_or_default();
        let sql = format!(
            "WITH parent_scope AS (
                 SELECT DISTINCT {parent_key} AS id FROM ({parent_scan}) WHERE {parent_key} = ?
             ), child_scope AS (
                 SELECT fact.{child_key} AS id, fact.{measure} AS value FROM ({child_scan}) fact
                 WHERE fact.{child_key} = ? {predicate}
             )
             SELECT p.id, count(c.id), CAST(sum(c.value) AS BIGINT)
             FROM parent_scope p LEFT JOIN child_scope c ON c.id = p.id GROUP BY p.id"
        );

        let conn = engine.shard_cache().read_connection()?;
        let mut stmt = conn.prepare(&sql).map_err(query_error)?;
        let parameters = [&key, &key]
            .into_iter()
            .chain(membership.as_ref().map(|(_, bound)| bound));
        let mut rows = stmt
            .query(params_from_iter(parameters))
            .map_err(query_error)?;
        let Some(row) = rows.next().map_err(query_error)? else {
            return Ok(None);
        };
        Ok(Some(ScopedGroupSum {
            id: key_type.read(row).map_err(query_error)?,
            count: row.get(1).map_err(query_error)?,
            sum: row.get(2).map_err(query_error)?,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use biocompass_core_traits::{CatalogId, OrgId};
    use std::collections::BTreeMap;

    #[test]
    fn membership_counts_scoped_measurements_once_on_the_pinned_four_tables() {
        use serde_json::json;
        let directory = tempfile::tempdir().unwrap();
        let catalog = CatalogId::new("qa/meter-path").unwrap();
        let mut engine = DuckLakeCatalogEngine::create_local_catalog(
            directory.path(), catalog.clone(), "Meters", "1", "Synthetic fixture", "",
            BTreeMap::from([
                ("meters".into(), vec!["meter_id".into()]),
                ("readings".into(), vec!["reading_id".into()]),
                ("labels".into(), vec!["label_id".into()]),
            ]),
            "original", |conn| conn.execute_batch(
                "CREATE TABLE dl.meters(meter_id VARCHAR);
                 INSERT INTO dl.meters VALUES ('east'), ('west'), ('empty');
                 CREATE TABLE dl.readings(reading_id BIGINT, meter_id VARCHAR, tag VARCHAR, watts BIGINT);
                 INSERT INTO dl.readings VALUES (1,'east','shared',3),(2,'east','shared',7),
                      (3,'east','other',100),(4,'west','shared',20);
                 CREATE TABLE dl.associations(tag VARCHAR, label_id BIGINT);
                 INSERT INTO dl.associations VALUES ('shared',10),('shared',10),('other',20),('shared',99);
                 CREATE TABLE dl.labels(label_id BIGINT);
                 INSERT INTO dl.labels VALUES (10),(20);"
            ).map_err(query_error)
        ).unwrap();
        let snapshot = engine.summary().latest_sealed.clone();
        let relation: ScopeRelation = serde_json::from_value(json!({
            "parent_table": "meters", "parent_key": "meter_id",
            "child_table": "readings", "child_foreign_key": "meter_id",
            "paths": {"tag_label": {"child_key":"tag", "bridge_table":"associations",
                "bridge_child_key":"tag", "bridge_dimension_key":"label_id",
                "dimension_table":"labels", "dimension_key":"label_id"}}
        }))
        .unwrap();
        let query: ScopedQueryDefinition = serde_json::from_value(json!({
            "measure": "watts", "where_exists": "tag_label"
        }))
        .unwrap();
        relation.validate(&snapshot).unwrap();
        relation.validate_query(&query, &snapshot).unwrap();
        let grant = |scope: &str| {
            let mut grant =
                DatasetGrant::compute_only_partner(OrgId::new(scope).unwrap(), catalog.clone());
            grant.row_scope = Some(FilterExpr::in_values("meter_id", [scope]));
            grant
        };
        engine
            .commit_mutation("add matching measurement", |connection| {
                connection
                    .execute_batch("INSERT INTO dl.readings VALUES (5,'east','shared',90)")
                    .map_err(query_error)
            })
            .unwrap();
        let run = |pin: &SealedSnapshot, scope: &str, params: serde_json::Value| {
            relation.execute_query(
                &query,
                &engine,
                pin.snapshot_id,
                &pin.dataset_digest,
                &grant(scope),
                params,
            )
        };
        let read = |pin: &SealedSnapshot, scope: &str, id: &str, related_id: i64| {
            serde_json::to_value(run(pin, scope, json!({"id":id,"related_id":related_id})).unwrap())
                .unwrap()
        };
        assert_eq!(
            read(&snapshot, "east", "east", 10),
            json!({"id":"east","count":2,"sum":10})
        );
        assert_eq!(
            read(&snapshot, "west", "west", 10),
            json!({"id":"west","count":1,"sum":20})
        );
        assert_eq!(read(&snapshot, "east", "west", 10), json!(null));
        assert_eq!(
            read(&snapshot, "empty", "empty", 10),
            json!({"id":"empty","count":0,"sum":null})
        );
        // The dangling bridge reference cannot reveal a dimension ID absent from the release.
        assert_eq!(
            read(&snapshot, "east", "east", 99),
            json!({"id":"east","count":0,"sum":null})
        );
        assert_eq!(
            read(&engine.summary().latest_sealed, "east", "east", 10),
            json!({"id":"east","count":3,"sum":100})
        );
        for invalid in [
            json!({"id":"east"}),
            json!({"id":"east","related_id":null}),
            json!({"id":"east","related_id":"10"}),
            json!({"id":"east","related_id":10,"tag":"shared"}),
        ] {
            assert!(matches!(
                run(&snapshot, "east", invalid),
                Err(CompassError::Validation(_))
            ));
        }
        let legacy = ScopedQueryDefinition::Sum("watts".into());
        assert!(matches!(
            relation.execute_query(
                &legacy,
                &engine,
                snapshot.snapshot_id,
                &snapshot.dataset_digest,
                &grant("east"),
                json!({"id":"east","related_id":10})
            ),
            Err(CompassError::Validation(_))
        ));
        let unknown = ScopedQueryDefinition::WhereExists {
            measure: "watts".into(),
            where_exists: "missing".into(),
        };
        assert!(matches!(
            relation.validate_query(&unknown, &snapshot),
            Err(CompassError::Validation(_))
        ));
        let mut wrong = relation.clone();
        wrong.paths.get_mut("tag_label").unwrap().bridge_child_key = "label_id".into();
        assert!(matches!(
            wrong.validate(&snapshot),
            Err(CompassError::Validation(_))
        ));
        let mut bad_grain = snapshot.clone();
        bad_grain
            .tables
            .iter_mut()
            .find(|t| t.schema.name == "readings")
            .unwrap()
            .schema
            .primary_key
            .clear();
        assert!(matches!(
            relation.validate(&bad_grain),
            Err(CompassError::Validation(_))
        ));
    }

    #[test]
    fn computes_a_join_against_two_scopes_on_the_same_pinned_release() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = CatalogId::new("qa/instrument-readings").unwrap();
        let engine = DuckLakeCatalogEngine::create_local_catalog(
            directory.path(),
            catalog.clone(),
            "Instruments and readings",
            "1",
            "Synthetic sensor fixture",
            "",
            BTreeMap::from([("instruments".into(), vec!["instrument_id".into()])]),
            "fixture",
            |conn| {
                conn.execute_batch(
                    "CREATE TABLE dl.instruments(instrument_id VARCHAR);
                     INSERT INTO dl.instruments VALUES ('I1'), ('I2'), ('I3');
                     CREATE TABLE dl.readings(instrument_id VARCHAR, reading BIGINT);
                     INSERT INTO dl.readings VALUES ('I1', 2), ('I1', 4), ('I2', 9);",
                )
                .map_err(query_error)
            },
        )
        .unwrap();
        let snapshot = &engine.summary().latest_sealed;
        let relation = ScopeRelation {
            parent_table: "instruments".into(),
            parent_key: "instrument_id".into(),
            child_table: "readings".into(),
            child_foreign_key: "instrument_id".into(),
            paths: BTreeMap::new(),
        };
        let query = ScopedJoinSum {
            measure: "reading".into(),
        };
        query.validate(&relation, snapshot).unwrap();

        let grant = |org: &str, keys: &[&str]| {
            let mut grant =
                DatasetGrant::compute_only_partner(OrgId::new(org).unwrap(), catalog.clone());
            grant.row_scope = Some(FilterExpr::in_values("instrument_id", keys.iter().copied()));
            grant
        };
        let org_a = grant("org-a", &["I1"]);
        let org_b = grant("org-b", &["I2"]);
        let execute = |grant: &DatasetGrant, id: &str| {
            query.execute(
                &relation,
                &engine,
                snapshot.snapshot_id,
                &snapshot.dataset_digest,
                grant,
                &ScopeKeyParameter { id: id.into() },
            )
        };
        let a = ScopedGroupSum {
            id: "I1".into(),
            count: 2,
            sum: Some(6),
        };
        let b = ScopedGroupSum {
            id: "I2".into(),
            count: 1,
            sum: Some(9),
        };
        assert_eq!(execute(&org_a, "I1").unwrap(), Some(a.clone()));
        assert_eq!(execute(&org_b, "I2").unwrap(), Some(b));
        assert_eq!(execute(&org_a, "I2").unwrap(), None);
        assert_eq!(execute(&org_b, "I1").unwrap(), None);
        assert_eq!(
            execute(&grant("org-a", &["I3"]), "I3").unwrap(),
            Some(ScopedGroupSum {
                id: "I3".into(),
                count: 0,
                sum: None,
            })
        );
        assert_eq!(execute(&org_a, "I1").unwrap(), Some(a));

        let mut unscoped = org_a.clone();
        unscoped.row_scope = None;
        assert!(matches!(
            execute(&unscoped, "I1"),
            Err(CompassError::AccessDenied(_))
        ));
        let mut wrong_column = org_a.clone();
        wrong_column.row_scope = Some(FilterExpr::in_values("other_id", ["I1"]));
        assert!(matches!(
            execute(&wrong_column, "I1"),
            Err(CompassError::AccessDenied(_))
        ));
        assert!(matches!(
            query.execute(
                &relation,
                &engine,
                snapshot.snapshot_id,
                "wrong digest",
                &org_a,
                &ScopeKeyParameter { id: "I1".into() },
            ),
            Err(CompassError::IntegrityViolation(_))
        ));
        let unsupported = ScopedJoinSum {
            measure: "instrument_id".into(),
        };
        assert!(matches!(
            unsupported.validate(&relation, snapshot),
            Err(CompassError::Validation(_))
        ));
    }

    #[test]
    fn signed_keys_preserve_scope_and_exact_json_results() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = CatalogId::new("qa/signed-readings").unwrap();
        let engine = DuckLakeCatalogEngine::create_local_catalog(
            directory.path(),
            catalog.clone(),
            "Instruments and readings",
            "1",
            "Synthetic sensor fixture",
            "",
            BTreeMap::from([
                ("instruments".into(), vec!["instrument_id".into()]),
                ("small_instruments".into(), vec!["instrument_id".into()]),
            ]),
            "fixture",
            |conn| {
                conn.execute_batch(
                    "CREATE TABLE dl.instruments(instrument_id BIGINT);
                     INSERT INTO dl.instruments VALUES (1), (2), (3), (-9007199254740991),
                        (9007199254740991), (9007199254740992), (9223372036854775807);
                     CREATE TABLE dl.readings(instrument_id BIGINT, reading BIGINT,
                        text_id VARCHAR, small_id SMALLINT);
                     INSERT INTO dl.readings VALUES (1, 2, '1', 1), (1, 4, '1', 1), (2, 9, '2', 2),
                        (-9007199254740991, 17, 'negative', 3),
                        (9007199254740991, 23, 'positive', 4),
                        (9007199254740992, 27, 'unsafe', 5);
                     CREATE TABLE dl.small_instruments(instrument_id SMALLINT);
                     INSERT INTO dl.small_instruments VALUES (1);",
                )
                .map_err(query_error)
            },
        )
        .unwrap();
        let snapshot = &engine.summary().latest_sealed;
        let relation = ScopeRelation {
            parent_table: "instruments".into(),
            parent_key: "instrument_id".into(),
            child_table: "readings".into(),
            child_foreign_key: "instrument_id".into(),
            paths: BTreeMap::new(),
        };
        let query = ScopedJoinSum {
            measure: "reading".into(),
        };
        query.validate(&relation, snapshot).unwrap();
        for child_key in ["text_id", "small_id"] {
            let incompatible = ScopeRelation {
                child_foreign_key: child_key.into(),
                ..relation.clone()
            };
            assert!(matches!(
                incompatible.validate(snapshot),
                Err(CompassError::Validation(_))
            ));
        }

        let grant = |keys: Vec<ScalarValue>| {
            let mut grant =
                DatasetGrant::compute_only_partner(OrgId::new("org-a").unwrap(), catalog.clone());
            grant.row_scope = Some(FilterExpr::in_values("instrument_id", keys));
            grant
        };
        let execute = |grant: &DatasetGrant, id: ScopeKey| {
            query.execute(
                &relation,
                &engine,
                snapshot.snapshot_id,
                &snapshot.dataset_digest,
                grant,
                &ScopeKeyParameter { id },
            )
        };

        let scope = grant(vec![1.into(), 3.into(), 4.into()]);
        assert_eq!(
            serde_json::to_value(execute(&scope, ScopeKey::Int(1)).unwrap().unwrap()).unwrap(),
            serde_json::json!({"id": 1, "count": 2, "sum": 6}),
        );
        assert_eq!(execute(&scope, ScopeKey::Int(2)).unwrap(), None); // forbidden
        assert_eq!(execute(&scope, ScopeKey::Int(4)).unwrap(), None); // absent
        assert_eq!(
            execute(&scope, ScopeKey::Int(3)).unwrap(),
            Some(ScopedGroupSum {
                id: ScopeKey::Int(3),
                count: 0,
                sum: None
            }),
        );
        assert!(matches!(
            execute(&scope, "1".into()),
            Err(CompassError::Validation(_))
        ));
        for invalid_key in [ScalarValue::Float(1.0), ScalarValue::Bool(true), "1".into()] {
            let invalid_scope = grant(vec![1.into(), invalid_key]);
            assert!(matches!(
                execute(&invalid_scope, ScopeKey::Int(1)),
                Err(CompassError::AccessDenied(_))
            ));
        }
        for invalid_json in [r#"{"id":1.0}"#, r#"{"id":true}"#, r#"{"id":null}"#] {
            assert!(serde_json::from_str::<ScopeKeyParameter>(invalid_json).is_err());
        }
        for (boundary, expected) in [(-9_007_199_254_740_991_i64, 17), (9_007_199_254_740_991, 23)] {
            let parameter: ScopeKeyParameter =
                serde_json::from_str(&format!(r#"{{"id":{boundary}}}"#)).unwrap();
            let scope = grant(vec![boundary.into()]);
            let result = execute(&scope, parameter.id).unwrap().unwrap();
            assert_eq!(
                serde_json::to_value(result).unwrap(),
                serde_json::json!({"id": boundary, "count": 1, "sum": expected})
            );
        }
        for outside in [-9_007_199_254_740_992, 9_007_199_254_740_992, i64::MAX] {
            assert!(matches!(
                execute(&scope, ScopeKey::Int(outside)),
                Err(CompassError::Validation(_))
            ));
            let unrepresentable = grant(vec![outside.into()]);
            assert!(matches!(
                execute(&unrepresentable, ScopeKey::Int(1)),
                Err(CompassError::AccessDenied(_))
            ));
        }
        assert!(
            relation
                .scoped_ids(&grant(vec![1.into(); 10_000]), snapshot)
                .is_ok()
        );
        assert!(matches!(
            relation.scoped_ids(&grant(vec![1.into(); 10_001]), snapshot),
            Err(CompassError::AccessDenied(_))
        ));

        let small = ScopeRelation {
            parent_table: "small_instruments".into(),
            child_foreign_key: "small_id".into(),
            ..relation
        };
        assert_eq!(
            query
                .execute(
                    &small,
                    &engine,
                    snapshot.snapshot_id,
                    &snapshot.dataset_digest,
                    &scope,
                    &ScopeKeyParameter {
                        id: ScopeKey::Int(1)
                    }
                )
                .unwrap(),
            Some(ScopedGroupSum {
                id: ScopeKey::Int(1),
                count: 2,
                sum: Some(6)
            }),
        );
        assert!(matches!(
            small.validate_keys(snapshot, &[ScopeKey::Int(32_768)]),
            Err(CompassError::Validation(_))
        ));
    }
}
