-- 会话级 MCP 访问档位与生产标记：NULL = 跟随所在分组（settings 键 mcp.group_policies）
-- 再回落全局（mcp.level / 非生产）。档位取值 readonly|standard|full。
ALTER TABLE sessions ADD COLUMN mcp_level TEXT;
ALTER TABLE sessions ADD COLUMN mcp_production INTEGER;
