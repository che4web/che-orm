pub fn database_url() -> String {
    std::env::var("CHE_ORM_DATABASE_URL").unwrap_or_else(|_| {
        #[cfg(feature = "sqlite")]
        {
            "template-single.db".into()
        }
        #[cfg(feature = "postgres")]
        {
            "postgresql://localhost/template_single".into()
        }
    })
}
