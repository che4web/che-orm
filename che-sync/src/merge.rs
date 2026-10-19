use crate::{
    model::Descriptor,
    protocol::{Record, Value},
};

/// Prefer local values at conflicted fields until the application resolves them.
pub(crate) fn merge(
    d: &Descriptor,
    base: Option<&Record>,
    local: Option<&Record>,
    remote: Option<&Record>,
) -> (Option<Record>, Vec<String>) {
    if local == remote {
        return (local.cloned(), vec![]);
    }
    if local == base {
        return (remote.cloned(), vec![]);
    }
    if remote == base {
        return (local.cloned(), vec![]);
    }
    let (Some(base), Some(local), Some(remote)) = (base, local, remote) else {
        return (local.cloned(), vec!["$entity".into()]);
    };
    let mut merged = remote.clone();
    let mut conflicts = Vec::new();
    for field in d.fields.iter().filter(|f| f.writable) {
        let b = &base[field.name];
        let l = &local[field.name];
        let r = &remote[field.name];
        if l != b && l != r {
            merged.insert(field.name.into(), l.clone());
            if r != b {
                conflicts.push(field.name.into());
            }
        }
    }
    (Some(merged), conflicts)
}

/// Explicit deletion must not be mistaken for an incoming row that has never
/// been materialized. Concurrent remote changes still require resolution.
pub(crate) fn delete(
    base: Option<&Record>,
    remote: Option<&Record>,
) -> (Option<Record>, Vec<String>) {
    let fields = if remote.is_some() && remote != base {
        vec!["$entity".into()]
    } else {
        vec![]
    };
    (None, fields)
}

pub(crate) fn patch(
    d: &Descriptor,
    base: Option<&Record>,
    local: Option<&Record>,
) -> Option<Record> {
    local.map(|record| {
        d.fields
            .iter()
            .filter(|f| f.writable && base.and_then(|b| b.get(f.name)) != record.get(f.name))
            .map(|f| {
                (
                    f.name.into(),
                    record.get(f.name).cloned().unwrap_or(Value::Null),
                )
            })
            .collect()
    })
}

/// Preserve edits made after preparing an operation. Sent writable values are
/// reconstructed from the frozen shadow and patch; server-owned fields may be
/// absent for a creation and are always taken from the canonical remote record.
pub(crate) fn acknowledge(
    d: &Descriptor,
    sent: Option<&Record>,
    local: Option<&Record>,
    remote: Option<&Record>,
) -> (Option<Record>, Vec<String>) {
    let unchanged = match (local, sent) {
        (Some(local), Some(sent)) => d
            .fields
            .iter()
            .filter(|f| f.writable)
            .all(|f| local.get(f.name) == sent.get(f.name)),
        (None, None) => true,
        _ => false,
    };
    if unchanged || local == remote {
        return (remote.cloned(), vec![]);
    }
    match (sent, local, remote) {
        (Some(sent), Some(local), Some(remote)) => {
            // Accepted server normalization is not a concurrent user edit.
            // Reapply subsequent local edits as the next intent, for example
            // when PostgreSQL rounded a sent timestamp to microseconds.
            let mut merged = remote.clone();
            for field in d.fields.iter().filter(|f| f.writable) {
                if local[field.name] != sent[field.name] {
                    merged.insert(field.name.into(), local[field.name].clone());
                }
            }
            (Some(merged), vec![])
        }
        (Some(_), None, Some(_)) => (None, vec![]),
        _ => (local.cloned(), vec!["$entity".into()]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Field, Registry, SyncModel};
    use che_orm::{Model, Uuid};

    #[derive(Debug, Model)]
    #[orm(table = "merge_dates")]
    struct Scheduled {
        #[orm(primary_key)]
        id: Uuid,
        scheduled: che_orm::time::OffsetDateTime,
        revision: i64,
        updated_at: che_orm::time::OffsetDateTime,
        deleted_at: Option<che_orm::time::OffsetDateTime>,
        sync_scope: String,
    }

    impl SyncModel for Scheduled {
        const SYNC_NAME: &'static str = "merge.scheduled";
        fn sync_fields() -> Vec<Field> {
            vec![
                Field::server_owned("id", "id"),
                Field::writable("scheduled", "scheduled"),
            ]
        }
    }

    #[test]
    fn acknowledged_timestamp_rounding_does_not_conflict_with_a_later_edit() {
        let registry = Registry::new().register::<Scheduled>().unwrap();
        let d = registry.get(Scheduled::SYNC_NAME).unwrap();
        let id = Uuid::now_v7();
        let make = |nanos| {
            Record::from([
                ("id".into(), Value::Uuid(id)),
                (
                    "scheduled".into(),
                    Value::DateTime(
                        che_orm::time::OffsetDateTime::from_unix_timestamp_nanos(nanos).unwrap(),
                    ),
                ),
            ])
        };
        let sent = make(1_234_567_890);
        let remote = make(1_234_567_000);
        let local = make(2_345_678_901);
        let (merged, conflicts) = acknowledge(d, Some(&sent), Some(&local), Some(&remote));
        assert_eq!(merged, Some(local));
        assert!(conflicts.is_empty());
    }
}
