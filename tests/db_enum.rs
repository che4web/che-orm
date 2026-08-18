#[derive(Debug, Clone, Copy, PartialEq, Eq, che_orm::DbEnum)]
enum ExternalStatus {
    Draft,
    #[db_enum(rename = "in_progress")]
    InProgress,
}

#[test]
fn db_enum_works_without_importing_the_trait() {
    assert_eq!(
        che_orm::serde_json::to_string(&ExternalStatus::InProgress).unwrap(),
        "\"in_progress\""
    );
    assert_eq!(
        <ExternalStatus as che_orm::DbEnum>::from_str("draft"),
        Some(ExternalStatus::Draft)
    );
}
