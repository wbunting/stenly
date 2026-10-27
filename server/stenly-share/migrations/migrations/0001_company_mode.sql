ALTER TABLE "shares" ADD COLUMN "owner_id" TEXT;
-- #[toasty::breakpoint]
CREATE INDEX IF NOT EXISTS "index_shares_by_owner_id" ON "shares" ("owner_id");
-- #[toasty::breakpoint]
CREATE TABLE IF NOT EXISTS "users" (
    "id" TEXT NOT NULL,
    "email" TEXT NOT NULL,
    "name" TEXT,
    "admin" BOOLEAN NOT NULL,
    "created_at" BIGINT NOT NULL,
    "last_login_at" BIGINT NOT NULL,
    PRIMARY KEY ("id")
);
-- #[toasty::breakpoint]
CREATE UNIQUE INDEX IF NOT EXISTS "index_users_by_email" ON "users" ("email");
-- #[toasty::breakpoint]
CREATE TABLE IF NOT EXISTS "sessions" (
    "token_hash" BLOB NOT NULL,
    "user_id" TEXT NOT NULL,
    "expires_at" BIGINT NOT NULL,
    PRIMARY KEY ("token_hash")
);
-- #[toasty::breakpoint]
CREATE INDEX IF NOT EXISTS "index_sessions_by_user_id" ON "sessions" ("user_id");
-- #[toasty::breakpoint]
CREATE TABLE IF NOT EXISTS "upload_tokens" (
    "id" TEXT NOT NULL,
    "token_hash" TEXT NOT NULL,
    "user_id" TEXT NOT NULL,
    "label" TEXT NOT NULL,
    "created_at" BIGINT NOT NULL,
    "last_used_at" BIGINT,
    PRIMARY KEY ("id")
);
-- #[toasty::breakpoint]
CREATE UNIQUE INDEX IF NOT EXISTS "index_upload_tokens_by_token_hash" ON "upload_tokens" ("token_hash");
-- #[toasty::breakpoint]
CREATE INDEX IF NOT EXISTS "index_upload_tokens_by_user_id" ON "upload_tokens" ("user_id");
-- #[toasty::breakpoint]
CREATE TABLE IF NOT EXISTS "settings" (
    "key" TEXT NOT NULL,
    "value" TEXT NOT NULL,
    PRIMARY KEY ("key")
);
-- #[toasty::breakpoint]
CREATE TABLE IF NOT EXISTS "view_days" (
    "day" BIGINT NOT NULL,
    "views" BIGINT NOT NULL,
    PRIMARY KEY ("day")
);
