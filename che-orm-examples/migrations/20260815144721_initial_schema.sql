-- Create "users" table
CREATE TABLE `users` (
  `id` integer NULL,
  `email` text NOT NULL,
  `name` text NOT NULL,
  `is_active` integer NOT NULL DEFAULT true,
  `created_at` text NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  `updated_at` text NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  PRIMARY KEY (`id`),
  CHECK (length(name) > 0)
);
-- Create index "users_email" to table: "users"
CREATE UNIQUE INDEX `users_email` ON `users` (`email`);
-- Create index "users_idx_0" to table: "users"
CREATE INDEX `users_idx_0` ON `users` (`name`);
-- Create "posts" table
CREATE TABLE `posts` (
  `id` integer NULL,
  `user_id` integer NOT NULL,
  `title` text NOT NULL,
  PRIMARY KEY (`id`),
  CONSTRAINT `0` FOREIGN KEY (`user_id`) REFERENCES `users` (`id`) ON UPDATE NO ACTION ON DELETE CASCADE
);
-- Create index "posts_idx_0" to table: "posts"
CREATE INDEX `posts_idx_0` ON `posts` (`user_id`);
