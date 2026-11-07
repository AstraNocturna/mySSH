-- 审计哈希链：prev_hash = 上一行 hash，hash = sha256(JSON[prev, ts, actor, session_id, action, detail])。
-- 迁移前的旧行两列为 NULL，链从第一条新行开始。
ALTER TABLE audit ADD COLUMN prev_hash TEXT;
ALTER TABLE audit ADD COLUMN hash TEXT;
