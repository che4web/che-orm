use che_orm::{Database, OrmError};

use template_modular::{models::Task, settings::database_url};

#[tokio::main]
async fn main() -> Result<(), OrmError> {
    let database = Database::connect(database_url()).await?;
    let task = database
        .create::<Task>()
        .set(Task::TITLE, "Try che-orm")
        .execute()
        .await?;
    println!("Created: {task:?}");

    let mut task = database
        .get::<Task>(task.id)
        .await?
        .ok_or_else(|| OrmError::Interaction("task no longer exists".into()))?;
    println!("Loaded: {task:?}");

    task.done = true;
    // save updates an existing row; use its result for managed timestamps.
    let task = database
        .save(&task)
        .await?
        .ok_or_else(|| OrmError::Interaction("task no longer exists".into()))?;
    println!("Saved: {task:?}");

    let tasks = database
        .query::<Task>()
        .filter(Task::DONE.eq(true))
        .order_by(Task::ID.desc())
        .limit(20)
        .all()
        .await?;
    println!("Completed tasks: {tasks:?}");

    let deleted = database.delete::<Task>(task.id).await?;
    println!("Deleted demo task: {deleted}");

    // save never recreates a row that has been deleted.
    assert!(database.save(&task).await?.is_none());
    Ok(())
}
