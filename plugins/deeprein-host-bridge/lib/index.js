import { timingSafeEqual } from 'node:crypto'
import { existsSync, readFileSync } from 'node:fs'
import { homedir } from 'node:os'
import { join } from 'node:path'

export const name = '@deeprein/host-bridge'
export const inject = ['webServer']

function getBridgeTokenPath() {
  if (process.env.DEEPREIN_BRIDGE_TOKEN_PATH) {
    return process.env.DEEPREIN_BRIDGE_TOKEN_PATH
  }
  if (process.env.DEEPREIN_APP_DATA_DIR) {
    return join(process.env.DEEPREIN_APP_DATA_DIR, 'bridge-token')
  }
  // 默认平台推导规则
  if (process.platform === 'win32') {
    const appData = process.env.APPDATA || join(homedir(), 'AppData', 'Roaming')
    return join(appData, 'com.deeprein.client', 'bridge-token')
  }
  if (process.platform === 'darwin') {
    return join(homedir(), 'Library', 'Application Support', 'com.deeprein.client', 'bridge-token')
  }
  return join(homedir(), '.config', 'com.deeprein.client', 'bridge-token')
}

function loadBridgeToken() {
  const tokenPath = getBridgeTokenPath()
  try {
    if (existsSync(tokenPath)) {
      return readFileSync(tokenPath, 'utf8').trim()
    }
  } catch {
    /* 无法读取 token 文件时返回空 */
  }
  return ''
}

function constantTimeEqual(a, b) {
  const bufA = Buffer.from(String(a || ''), 'utf8')
  const bufB = Buffer.from(String(b || ''), 'utf8')
  if (bufA.length === 0 || bufB.length === 0) return false
  if (bufA.length !== bufB.length) return false
  return timingSafeEqual(bufA, bufB)
}

function sendJson(res, statusCode, value) {
  if (res.headersSent) return
  res.writeHead(statusCode || 200, { 'Content-Type': 'application/json' })
  res.end(JSON.stringify(value))
}

function sendText(res, statusCode, text) {
  if (res.headersSent) return
  res.writeHead(statusCode || 200, { 'Content-Type': 'text/plain; charset=utf-8' })
  res.end(String(text || ''))
}

// 与 cordis 的 FiberState 枚举严格对齐（cordis/src/fiber.ts:147-154）：
// PENDING=0, LOADING=1, ACTIVE=2, FAILED=3, DISPOSED=4, UNLOADING=5
// 漏掉任一项会让该状态掉进下方兜底、被误标成 unresolved，排障时误导人。
const FIBER_PHASE = ['pending', 'loading', 'active', 'failed', 'disposed', 'unloading']

export function apply(ctx) {
  const disposers = []

  // 认证中间件函数：校验 Bearer Token
  function authenticate(req, res) {
    const expectedToken = loadBridgeToken()
    if (!expectedToken) {
      sendJson(res, 401, { ok: false, error: 'unauthorized' })
      return false
    }
    const authHeader = (req.headers && (req.headers.authorization || req.headers.Authorization)) || ''
    const match = authHeader.match(/^Bearer\s+(.+)$/i)
    const token = match ? match[1] : ''
    if (!token || !constantTimeEqual(token, expectedToken)) {
      sendJson(res, 401, { ok: false, error: 'unauthorized' })
      return false
    }
    return true
  }

  // GET /__deeprein/health 端点
  disposers.push(
    ctx.webServer.register({
      kind: 'exact',
      path: '/__deeprein/health',
      handler: async (req, res) => {
        if (req.method !== 'GET') {
          return sendText(res, 405, 'method not allowed')
        }
        if (!authenticate(req, res)) return

        const problems = []
        let pluginsStatus = null

        try {
          if (ctx.loader && typeof ctx.loader.entries === 'function') {
            const entries = []
            for (const entry of ctx.loader.entries()) {
              if (entry.options && entry.options.group) continue
              const fiberState = entry.fiber ? entry.fiber.state : null
              const phase = fiberState !== null && FIBER_PHASE[fiberState] ? FIBER_PHASE[fiberState] : (entry.disabled ? 'disabled' : 'unresolved')
              const name = entry.options && entry.options.name ? entry.options.name : (entry.id || 'anonymous')
              const isFailed = (!entry.disabled && (entry.fiber === undefined || fiberState === 3)) // FIBER_FAILED = 3
              if (isFailed) {
                problems.push(`plugin failed: ${name} (${phase})`)
              }
              entries.push({
                id: entry.id,
                name,
                enabled: !entry.disabled,
                phase,
                failed: isFailed,
              })
            }
            pluginsStatus = {
              total: entries.length,
              entries,
            }
          }
        } catch (err) {
          problems.push(`failed to inspect loader entries: ${err.message || err}`)
        }

        let invariantsStatus = null
        try {
          const invariants = ctx.get('invariants')
          if (invariants && typeof invariants === 'object') {
            invariantsStatus = {
              enabled: Boolean(invariants.enabled),
              registrations: invariants.registrations ? invariants.registrations.size : null,
            }
          }
        } catch {
          /* invariants 探测降级 */
        }

        const isOk = problems.length === 0

        sendJson(res, 200, {
          ok: isOk,
          problems,
          backend: {
            pid: process.pid,
            nodeVersion: process.version,
            platform: process.platform,
            arch: process.arch,
            uptimeSec: Math.floor(process.uptime()),
          },
          plugins: pluginsStatus,
          invariants: invariantsStatus,
        })
      },
    }),
  )

  return () => {
    for (const dispose of disposers.splice(0)) {
      try {
        dispose()
      } catch {
        /* ignore */
      }
    }
  }
}
