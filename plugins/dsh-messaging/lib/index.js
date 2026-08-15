// @deeprein/dsh-messaging 启动壳
//
// 职责：
//   1) 把伴随脚本同步到 ~/.dsh-messaging/companion，并预置用户级 config.json
//      （nodePath=当前进程 Node、wsModulePath=profile 内 ws、companionDir=用户目录）；
//   2) 等宿主服务（agents / dynamicCordisRunner）就绪后，创建专属 bootstrap 会话；
//   3) 用该会话 cordis_define 注册 dsh-messaging 动态插件，并直接激活 Host 半区
//      （requestId=null 的直连路径自动批准 Client 包，无需用户在 UI 上点审批）。
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
} from 'node:fs'
import { createRequire } from 'node:module'
import { homedir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const require = createRequire(import.meta.url)
const pkgDir = join(dirname(fileURLToPath(import.meta.url)), '..')

const ID_PREFIX = 'dshmsg'
const SESSION_ID = 'deeprein-dsh-messaging'
const RETRY_MS = 1500
const CONFIG_ROOT_MARKER = "const CONFIG_ROOT_OVERRIDE = ''"

// cordis 严格模式：访问 ctx.interval（timer 插件提供）必须显式声明依赖
export const inject = ['timer']
export const name = '@deeprein/dsh-messaging'

function ensureRuntimeHome() {
  // 配置根目录 = 用户主目录：host.js 会在 <root>/.dsh-messaging/config.json 读写配置
  const root = homedir()
  const configDir = join(root, '.dsh-messaging')
  const companionDir = join(configDir, 'companion')
  mkdirSync(companionDir, { recursive: true })
  for (const file of ['crypto-helper.cjs', 'discord-gateway.cjs']) {
    const src = join(pkgDir, 'companion', file)
    if (existsSync(src)) copyFileSync(src, join(companionDir, file))
  }
  const configPath = join(configDir, 'config.json')
  if (!existsSync(configPath)) {
    let wsModulePath = ''
    try {
      wsModulePath = require.resolve('ws')
    } catch {
      // profile 未解析到 ws 时留空：Discord 网关不可用，其余渠道不受影响
    }
    writeFileSync(
      configPath,
      JSON.stringify(
        {
          workspaceRoot: root,
          runtime: {
            nodePath: process.execPath,
            wsModulePath,
            companionDir,
          },
        },
        null,
        2,
      ),
    )
  }
  return root
}

export function apply(ctx) {
  const home = ensureRuntimeHome()
  let started = false
  const timer = ctx.interval(async () => {
    if (started) return
    let runner
    let agents
    try {
      runner = ctx.get('dynamicCordisRunner')
      agents = ctx.get('agents')
    } catch {
      return // 宿主服务尚未挂载，下轮重试
    }
    if (!runner || !agents) return
    started = true
    try {
      const handle = await agents.create({
        sessionId: SESSION_ID,
        meta: { cwd: home },
        agentOptions: {},
      })
      const hostSource = readFileSync(join(pkgDir, 'dynamic', 'host.js'), 'utf8').replace(
        CONFIG_ROOT_MARKER,
        `const CONFIG_ROOT_OVERRIDE = ${JSON.stringify(home)}`,
      )
      const clientSource = readFileSync(join(pkgDir, 'dynamic', 'client.js'), 'utf8')
      const defined = runner.define({
        sessionId: handle.agent.id,
        plugin: { kind: 'new', idPrefix: ID_PREFIX },
        name: 'dsh-messaging',
        purpose:
          'DeepRein 内置消息渠道网关：OneBot v11、Telegram、Discord、Slack、飞书/Lark、企业微信、个人微信',
        code: { host: hostSource, client: clientSource },
      })
      const outcome = await runner.runHostHalf(
        handle.agent,
        defined.pluginId,
        defined.packageId,
        'run',
        null,
        false,
      )
      console.log(
        `[dsh-messaging] activated ${defined.pluginId}/${defined.packageId}:`,
        outcome && outcome.status,
      )
    } catch (error) {
      console.error('[dsh-messaging] bootstrap failed:', error)
      started = false // 会话/激活竞态时下轮重试
    }
  }, RETRY_MS)
  return () => {
    if (timer) timer()
  }
}
