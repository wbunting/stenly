CREATE TABLE IF NOT EXISTS "shares" (
    "id" TEXT NOT NULL,
    "owner_hash" TEXT NOT NULL,
    "title" TEXT,
    "size" BIGINT NOT NULL,
    "created_at" BIGINT NOT NULL,
    "updated_at" BIGINT NOT NULL,
    "expires_at" BIGINT NOT NULL,
    "revoked_at" BIGINT,
    "views" BIGINT NOT NULL,
    "purged" BOOLEAN NOT NULL,
    PRIMARY KEY ("id")
);
-- #[toasty::breakpoint]
CREATE INDEX IF NOT EXISTS "index_shares_by_expires_at" ON "shares" ("expires_at");
-- #[toasty::breakpoint]
CREATE TABLE IF NOT EXISTS "share_blobs" (
    "share_id" TEXT NOT NULL,
    "ciphertext" BLOB NOT NULL,
    PRIMARY KEY ("share_id")
);
