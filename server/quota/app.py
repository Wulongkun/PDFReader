"""PDFReader 额度服务（试用 + Pro 固定赠送页数，统一按页计费）。

极小的 FastAPI 应用，负责：
- register：机器指纹（machine_id）换一个绑定机器的 key（幂等，初始赠送试用页数）；
- consume：原子扣减页数（额度不足返回 ok=false）；
- status：查询剩余额度；
- activate-pro：验签 Pro 激活票据后，把 granted 提到 Pro 赠送页数（幂等）；
- topup：作者密钥鉴权后充值。

额度以 machine_id 为主键存在 PostgreSQL —— 删配置/重装后重新 register 命中同一
machine_id，返回同一 key 与已扣减额度，无法通过删文件/改机器号重置。
"""

import os
import time
import secrets
import base64
import json
from contextlib import asynccontextmanager

import psycopg2
import psycopg2.extras
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
from fastapi import FastAPI, Header, HTTPException
from pydantic import BaseModel

DATABASE_URL = os.environ.get("DATABASE_URL")
if not DATABASE_URL:
    raise RuntimeError("缺少 DATABASE_URL 环境变量（形如 postgresql://user:pass@host:5432/quota）")

# 试用（非 Pro）默认赠送页数；Pro 激活后 granted 提到 PRO_GRANT（固定赠送，买断）。
GRANTED = int(os.environ.get("TRIAL_GRANTED", "50"))
PRO_GRANT = int(os.environ.get("PRO_GRANT", "1000"))
TOPUP_SECRET = os.environ.get("TOPUP_SECRET", "")

# 激活票据公钥（与 App 端 license.rs 的 PUBLIC_KEY_HEX 一致，只验签、无私钥）。
PUBLIC_KEY_HEX = "5051895d69d78206bfb79b0bd92a7fe1ba815ef2eee57a9b9b39eaf541674751"


def _verify_receipt(receipt: str, machine_id: str) -> bool:
    """校验激活票据：base64url(payload ‖ 64B Ed25519 签名)，且 type=receipt、绑定本机。"""
    try:
        raw = base64.urlsafe_b64decode(receipt + "=" * (-len(receipt) % 4))
        if len(raw) < 64:
            return False
        payload, sig = raw[:-64], raw[-64:]
        Ed25519PublicKey.from_public_bytes(bytes.fromhex(PUBLIC_KEY_HEX)).verify(sig, payload)
        data = json.loads(payload)
        return data.get("type") == "receipt" and data.get("machine_id") == machine_id
    except Exception:
        return False


def _conn():
    return psycopg2.connect(DATABASE_URL)


def _alloc_account_no(cur) -> int:
    """分配一个未使用的 6 位识别号（100000~999999，绑定 machine_id、不重复）。"""
    for _ in range(100):
        n = secrets.randbelow(900000) + 100000
        cur.execute("SELECT 1 FROM trial_accounts WHERE account_no = %s", (n,))
        if cur.fetchone() is None:
            return n
    raise RuntimeError("识别号分配失败（尝试 100 次均冲突）")


def _backfill_account_no() -> None:
    """为迁移前的老账号补齐 6 位识别号（只处理 account_no 为 NULL 的行）。"""
    with _conn() as c:
        with c.cursor(cursor_factory=psycopg2.extras.RealDictCursor) as cur:
            cur.execute("SELECT machine_id FROM trial_accounts WHERE account_no IS NULL")
            rows = cur.fetchall()
        c.commit()
    for r in rows:
        with _conn() as c2:
            with c2.cursor() as cur2:
                n = _alloc_account_no(cur2)
                cur2.execute(
                    "UPDATE trial_accounts SET account_no = %s "
                    "WHERE machine_id = %s AND account_no IS NULL",
                    (n, r["machine_id"]),
                )
            c2.commit()


def _init_schema():
    with _conn() as c:
        with c.cursor() as cur:
            cur.execute(
                """
                CREATE TABLE IF NOT EXISTS trial_accounts (
                    machine_id text PRIMARY KEY,
                    trial_key  text NOT NULL,
                    granted    int  NOT NULL,
                    used       int  NOT NULL DEFAULT 0,
                    created_at bigint NOT NULL,
                    last_seen  bigint
                )
                """
            )
            # 识别号：6 位数字，绑定 machine_id、唯一（充值/查账时用它代替冗长的机器码）。
            cur.execute("ALTER TABLE trial_accounts ADD COLUMN IF NOT EXISTS account_no INTEGER")
            cur.execute(
                "CREATE UNIQUE INDEX IF NOT EXISTS trial_accounts_account_no_key "
                "ON trial_accounts (account_no)"
            )
        c.commit()
    _backfill_account_no()


@asynccontextmanager
async def lifespan(_app: FastAPI):
    _init_schema()
    yield


app = FastAPI(lifespan=lifespan)


def _row_to_payload(row):
    return {
        "trial_key": row["trial_key"],
        "granted": row["granted"],
        "used": row["used"],
        "remaining": max(row["granted"] - row["used"], 0),
        "account_no": row.get("account_no"),
    }


class RegisterBody(BaseModel):
    machine_id: str


class ConsumeBody(BaseModel):
    machine_id: str
    trial_key: str
    pages: int = 1


class TopupBody(BaseModel):
    machine_id: str = ""
    account_no: int = 0
    pages: int


class ActivateProBody(BaseModel):
    machine_id: str
    trial_key: str
    receipt: str


@app.post("/register")
def register(body: RegisterBody):
    machine_id = body.machine_id.strip()
    if not machine_id:
        raise HTTPException(400, "machine_id 不能为空")

    with _conn() as c:
        with c.cursor(cursor_factory=psycopg2.extras.RealDictCursor) as cur:
            cur.execute(
                "SELECT trial_key, granted, used, account_no FROM trial_accounts WHERE machine_id = %s",
                (machine_id,),
            )
            row = cur.fetchone()
            if row is None:
                trial_key = secrets.token_urlsafe(24)
                account_no = _alloc_account_no(cur)
                cur.execute(
                    "INSERT INTO trial_accounts (machine_id, trial_key, granted, used, created_at, account_no) "
                    "VALUES (%s, %s, %s, 0, %s, %s) ON CONFLICT (machine_id) DO NOTHING",
                    (machine_id, trial_key, GRANTED, int(time.time()), account_no),
                )
        c.commit()

    # 并发下可能另一个请求已插入：重新读取返回，保证幂等（同 machine_id 返回同 key）。
    with _conn() as c:
        with c.cursor(cursor_factory=psycopg2.extras.RealDictCursor) as cur:
            cur.execute(
                "SELECT trial_key, granted, used, account_no FROM trial_accounts WHERE machine_id = %s",
                (machine_id,),
            )
            row = cur.fetchone()
        c.commit()
    if row is None:
        raise HTTPException(500, "注册失败")
    return _row_to_payload(row)


@app.post("/consume")
def consume(body: ConsumeBody):
    pages = max(int(body.pages), 1)
    machine_id = body.machine_id.strip()
    trial_key = body.trial_key.strip()

    with _conn() as c:
        with c.cursor(cursor_factory=psycopg2.extras.RealDictCursor) as cur:
            cur.execute(
                """
                UPDATE trial_accounts
                SET used = used + %s, last_seen = %s
                WHERE machine_id = %s AND trial_key = %s AND used + %s <= granted
                RETURNING granted, used
                """,
                (pages, int(time.time()), machine_id, trial_key, pages),
            )
            row = cur.fetchone()
            if row is not None:
                c.commit()
                return {"ok": True, "remaining": max(row["granted"] - row["used"], 0)}

            cur.execute(
                "SELECT trial_key, granted, used FROM trial_accounts WHERE machine_id = %s",
                (machine_id,),
            )
            acc = cur.fetchone()
        c.commit()

    if acc is None:
        return {"ok": False, "remaining": 0, "error": "试用账号不存在，请重新启动应用注册"}
    if acc["trial_key"] != trial_key:
        return {"ok": False, "remaining": 0, "error": "试用 key 与本机不匹配"}
    return {"ok": False, "remaining": 0, "error": "额度已用完"}


@app.get("/status")
def status(machine_id: str, trial_key: str):
    with _conn() as c:
        with c.cursor(cursor_factory=psycopg2.extras.RealDictCursor) as cur:
            cur.execute(
                "SELECT granted, used, account_no FROM trial_accounts WHERE machine_id = %s AND trial_key = %s",
                (machine_id.strip(), trial_key.strip()),
            )
            row = cur.fetchone()
        c.commit()
    if row is None:
        raise HTTPException(404, "试用账号不存在")
    return {
        "granted": row["granted"],
        "used": row["used"],
        "remaining": max(row["granted"] - row["used"], 0),
        "account_no": row["account_no"],
    }


@app.post("/activate-pro")
def activate_pro(body: ActivateProBody):
    """Pro 激活后授予固定赠送页数：验签票据 + 核对机器绑定，幂等（GREATEST 不重复叠加）。"""
    machine_id = body.machine_id.strip()
    trial_key = body.trial_key.strip()
    if not _verify_receipt(body.receipt, machine_id):
        raise HTTPException(403, "激活票据无效")
    with _conn() as c:
        with c.cursor(cursor_factory=psycopg2.extras.RealDictCursor) as cur:
            cur.execute(
                "UPDATE trial_accounts SET granted = GREATEST(granted, %s) "
                "WHERE machine_id = %s AND trial_key = %s RETURNING granted, used",
                (PRO_GRANT, machine_id, trial_key),
            )
            row = cur.fetchone()
        c.commit()
    if row is None:
        raise HTTPException(404, "额度账号不存在，请重新启动应用注册")
    return {
        "granted": row["granted"],
        "used": row["used"],
        "remaining": max(row["granted"] - row["used"], 0),
    }


@app.post("/topup")
def topup(body: TopupBody, x_topup_secret: str = Header(default="")):
    if not TOPUP_SECRET or x_topup_secret != TOPUP_SECRET:
        raise HTTPException(403, "无权充值")
    pages = max(int(body.pages), 0)
    account_no = int(body.account_no or 0)
    machine_id = (body.machine_id or "").strip()
    with _conn() as c:
        with c.cursor(cursor_factory=psycopg2.extras.RealDictCursor) as cur:
            if account_no > 0:
                cur.execute(
                    "UPDATE trial_accounts SET granted = granted + %s WHERE account_no = %s "
                    "RETURNING machine_id, account_no, granted, used",
                    (pages, account_no),
                )
            else:
                cur.execute(
                    "UPDATE trial_accounts SET granted = granted + %s WHERE machine_id = %s "
                    "RETURNING machine_id, account_no, granted, used",
                    (pages, machine_id),
                )
            row = cur.fetchone()
        c.commit()
    if row is None:
        raise HTTPException(404, "账号不存在（确认识别号/机器码，或用户先运行一次应用完成注册）")
    return {
        "account_no": row["account_no"],
        "granted": row["granted"],
        "used": row["used"],
        "remaining": max(row["granted"] - row["used"], 0),
    }


@app.get("/admin/accounts")
def admin_accounts(x_topup_secret: str = Header(default="")):
    """作者后台：列出全部账号（识别号 + 机器码 + 额度），按最近活跃倒序。"""
    if not TOPUP_SECRET or x_topup_secret != TOPUP_SECRET:
        raise HTTPException(403, "无权访问")
    with _conn() as c:
        with c.cursor(cursor_factory=psycopg2.extras.RealDictCursor) as cur:
            cur.execute(
                "SELECT machine_id, account_no, granted, used, created_at, last_seen "
                "FROM trial_accounts ORDER BY COALESCE(last_seen, created_at) DESC"
            )
            rows = cur.fetchall()
        c.commit()
    accounts = [
        {
            "machine_id": r["machine_id"],
            "account_no": r["account_no"],
            "granted": r["granted"],
            "used": r["used"],
            "remaining": max(r["granted"] - r["used"], 0),
            "created_at": r["created_at"],
            "last_seen": r["last_seen"],
        }
        for r in rows
    ]
    return {"accounts": accounts, "total": len(accounts)}
