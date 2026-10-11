# 额度服务（试用 + Pro 固定赠送页数）

给 PDFReader 提供「机器绑定 key + 按页计费 + 充值」的轻量后端。复用服务器上已有的 PostgreSQL（新建独立库 `quota`，不动 new-api 的表）。

## 接口

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| POST | `/register` | `{machine_id}` → 幂等签发机器绑定 `trial_key`，返回 `{trial_key, granted, used, remaining}` |
| POST | `/consume` | `{machine_id, trial_key, pages}` → 原子扣页；额度不足返回 `{ok:false, error:"额度已用完"}` |
| GET | `/status` | `?machine_id=&trial_key=` → `{granted, used, remaining}` |
| POST | `/activate-pro` | `{machine_id, trial_key, receipt}` → 验签 Pro 激活票据后把 `granted` 提到 `PRO_GRANT`（幂等） |
| POST | `/topup` | `{machine_id, pages}` + 头 `X-Topup-Secret` → 作者充值 |

## 部署（服务器 47.117.82.148）

1. 在 postgres 里建独立库（复用现有 `postgres` 容器）：
   ```bash
   docker exec -i postgres psql -U newapi -c "CREATE DATABASE quota;"
   ```

2. 构建并运行容器（加入与 `postgres` 相同的 docker 网络）：
   ```bash
   docker build -t pdfreader-quota .
   docker run -d --name pdfreader-quota \
     --network <docker-network> \
     -e DATABASE_URL=postgresql://newapi:<PASS>@postgres:5432/quota \
     -e TRIAL_GRANTED=50 \
     -e PRO_GRANT=1000 \
     -e TOPUP_SECRET=<随机长密钥> \
     pdfreader-quota
   ```

3. nginx 反代（挂到作者域名，如 `https://yuanjingzh.cn/quota/`），转发到容器 8000 端口。

## 作者充值

```bash
curl -X POST https://yuanjingzh.cn/quota/topup \
  -H "X-Topup-Secret: <TOPUP_SECRET>" \
  -H "Content-Type: application/json" \
  -d '{"machine_id":"<用户的 machine_id>","pages":100}'
```

`machine_id` 从用户配置文件 `%APPDATA%/PDFReader/config.json` 的 `license.machine_id`（试用注册后同目录下也会存）或数据库 `trial_accounts` 表读取。

## 防破解要点

- `machine_id` 为主键：删配置 / 重装 / 改机器号都无法重置额度（`register` 命中同 machine_id 返回同 key 与已扣额度）。
- `consume` 用单条 `UPDATE … WHERE used + N <= granted` 原子扣减，无并发超扣。
- `/activate-pro` 用 Ed25519 公钥验签激活票据（`PUBLIC_KEY_HEX` 与 App 端 `license.rs` 一致），伪造激活码无法白拿 Pro 赠送页数。
- key 对用户不展示（仅存在 config.json 供校验，UI 不显示字符）。
