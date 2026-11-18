import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { useAppStore } from '../state/app-store';
import { TunnelEditor } from './TunnelEditor';
import { Dialog } from './Dialog';
import { Icon } from './Icon';
import { ConfirmDialog } from './ConfirmDialog';
import { MCP_LEVELS, MCP_PERM_GROUPS, setPermMode } from './mcp-options';
import {
  START_MODE_LABEL,
  TUNNEL_KIND_KEY,
  TUNNEL_STATUS_KEY,
  startModeOf,
  tunnelDisplayName,
} from '../state/tunnel-utils';
import { useT, type MsgKey } from '../i18n';
import type { AuthSpec, SessionRecord, TestConnectRequest, TunnelDef } from '../term/types';

type AuthKind = AuthSpec['type'];
type EditorTab = 'basic' | 'tunnels' | 'mcp';
/** 终端编码选项（encoding_rs 标签；utf-8 = 直通不转码） */
const ENCODING_OPTIONS = [
  ['utf-8', 'dialogs.encodingUtf8'],
  ['gbk', 'dialogs.encodingGbk'],
  ['gb18030', 'dialogs.encodingGb18030'],
  ['big5', 'dialogs.encodingBig5'],
  ['shift_jis', 'dialogs.encodingShiftJis'],
  ['euc-kr', 'dialogs.encodingEucKr'],
] as const;

const TAB_LABEL: Record<EditorTab, MsgKey> = {
  basic: 'dialogs.tabBasic',
  tunnels: 'dialogs.tabTunnels',
  mcp: 'dialogs.tabMcp',
};

// TestConnectRequest 唯一源头 = Rust（term/bindings，经 types.ts re-export）

/** session_test_connect 返回（Ok 值，不 Err） */
interface TestConnectResult {
  ok: boolean;
  latencyMs?: number;
  error?: string;
}

type TestState =
  | { phase: 'idle' }
  | { phase: 'testing' }
  | { phase: 'ok'; latencyMs: number | null }
  | { phase: 'err'; message: string };

/** 标签颜色预设（选「无」= null） */
const PRESET_COLORS = [
  '#e5484d',
  '#e93d82',
  '#ab4aba',
  '#6e56cf',
  '#3e63dd',
  '#0090ff',
  '#12a594',
  '#30a46c',
];

const inputCls =
  'h-9 w-full rounded-md border border-neutral-700 bg-neutral-800 px-3 text-sm text-neutral-100 outline-none placeholder:text-neutral-500 focus:border-blue-600';
const SWATCH_ACTIVE = 'ring-2 ring-neutral-200 ring-offset-2 ring-offset-neutral-900';

/** 表单项：标签 + 必填星号 + 内联错误；包 label 让点击标签聚焦控件 */
function Field({
  label,
  required,
  error,
  className = '',
  children,
}: {
  label: string;
  required?: boolean;
  error?: string;
  className?: string;
  children: React.ReactNode;
}) {
  return (
    <label className={`block ${className}`}>
      <span className="mb-1.5 block text-xs font-medium text-neutral-400">
        {label}
        {required && <span className="ml-0.5 text-red-400">*</span>}
      </span>
      {children}
      {error && <span className="mt-1 block text-xs text-red-400">{error}</span>}
    </label>
  );
}

/** 密码框 + 显示/隐藏切换（切换按钮不进 Tab 顺序，避免打断表单键盘流） */
function PasswordInput({
  value,
  onChange,
  autoComplete = 'off',
  placeholder,
}: {
  value: string;
  onChange: (v: string) => void;
  autoComplete?: string;
  placeholder?: string;
}) {
  const t = useT();
  const [shown, setShown] = useState(false);
  return (
    <div className="relative">
      <input
        className={`${inputCls} pr-10`}
        type={shown ? 'text' : 'password'}
        value={value}
        autoComplete={autoComplete}
        spellCheck={false}
        placeholder={placeholder}
        onChange={(e) => onChange(e.target.value)}
      />
      <button
        type="button"
        tabIndex={-1}
        className="absolute top-1/2 right-1 flex h-7 w-7 -translate-y-1/2 items-center justify-center rounded text-neutral-400 hover:bg-neutral-700 hover:text-neutral-100"
        title={shown ? t('dialogs.hidePassword') : t('dialogs.showPassword')}
        aria-label={shown ? t('dialogs.hidePassword') : t('dialogs.showPassword')}
        onClick={() => setShown((v) => !v)}
      >
        <Icon name={shown ? 'eyeOff' : 'eye'} size={16} />
      </button>
    </div>
  );
}

/** 新建/编辑会话对话框。key=id 重挂载重置表单（editing 预填）。 */
export function ConnectDialog() {
  const show = useAppStore((s) => s.showConnect);
  const editing = useAppStore((s) => s.editing);
  const presetGroup = useAppStore((s) => s.connectPreset);
  if (!show) return null;
  // key 含 preset：分组菜单「新建连接」重挂载时重置表单并预填分组
  return (
    <ConnectForm
      key={editing?.id ?? `new-${presetGroup ?? ''}`}
      initial={editing}
      presetGroup={presetGroup}
    />
  );
}

function ConnectForm({
  initial,
  presetGroup,
}: {
  initial: SessionRecord | null;
  presetGroup: string | null;
}) {
  const close = useAppStore((s) => s.closeConnect);
  const connect = useAppStore((s) => s.connect);
  const connectBySession = useAppStore((s) => s.connectBySession);
  const notify = useAppStore((s) => s.notify);
  const loadSessions = useAppStore((s) => s.loadSessions);
  const t = useT();

  const [tab, setTab] = useState<EditorTab>('basic');
  const [name, setName] = useState(initial?.name ?? '');
  // 会话类型：创建后锁定（认证/凭据/隧道语义完全不同，互转会留下脏数据）
  const [sessKind, setSessKind] = useState<'ssh' | 'local'>(initial?.kind ?? 'ssh');
  // local 表单字段
  const [workdir, setWorkdir] = useState(initial?.workdir ?? '');
  const [shell, setShell] = useState(initial?.shell ?? '');
  const [command, setCommand] = useState(initial?.command ?? '');
  const [nameTouched, setNameTouched] = useState(false);
  const [color, setColor] = useState<string | null>(initial?.color ?? null);
  const [encoding, setEncoding] = useState(initial?.encoding ?? 'utf-8');
  const [host, setHost] = useState(initial?.host ?? '');
  // 端口用文本态：数字态下清空会被回填 22，无法重新输入
  const [portText, setPortText] = useState(String(initial?.port || 22));
  const port = Number(portText);
  const portOk = Number.isInteger(port) && port >= 1 && port <= 65535;
  const [user, setUser] = useState(initial?.username ?? '');
  const [kind, setKind] = useState<AuthKind>(
    initial?.authType === 'publickey'
      ? 'publicKey'
      : initial?.authType === 'keyboard-interactive'
        ? 'keyboardInteractive'
        : (initial?.authType ?? 'password'),
  );
  const [password, setPassword] = useState('');
  // su 二级登录：目标用户名 + 密码（密码仅存保险库 kind=suPassword，不回读表单）
  const [suUser, setSuUser] = useState(initial?.suUser ?? '');
  // MCP 工具权限覆盖：稀疏映射，键缺席 = 跟随全局（设置 → MCP 工具权限）
  const [mcpPerms, setMcpPerms] = useState<Record<string, boolean>>(initial?.mcpPerms ?? {});
  // MCP 档位/生产标记覆盖：null = 继承分组策略/全局
  const [mcpLevel, setMcpLevel] = useState<string | null>(initial?.mcpLevel ?? null);
  const [mcpProduction, setMcpProduction] = useState<boolean | null>(
    initial?.mcpProduction ?? null,
  );
  const [suPassword, setSuPassword] = useState('');
  // 登录宏：进 shell 后逐行自动执行（多行文本；仅边综 trim 判空，正文原样存）
  const [loginMacro, setLoginMacro] = useState(initial?.loginMacro ?? '');
  const [keyPath, setKeyPath] = useState(initial?.keyPath ?? '');
  const [passphrase, setPassphrase] = useState('');
  // 本对话框是完整编辑器，默认落库（一次性连接走 QuickConnectDialog）；默认保存后名称/颜色设置可见
  const [save, setSave] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [test, setTest] = useState<TestState>({ phase: 'idle' });

  // 新建时名称跟随 用户@主机；用户手动改过后停止跟随（编辑已有会话不跟随）。
  // 在 host/user 的 onChange 里同步推导，避免 effect 里同步 setState（react-hooks v6）
  const followName = (u: string, h: string) => {
    if (initial || nameTouched) return;
    const ut = u.trim();
    const ht = h.trim();
    setName(ut && ht ? `${ut}@${ht}` : '');
  };

  /** 按表单当前值测试连接；不改表单状态，结果内联显示 */
  const testConnect = async () => {
    setTest({ phase: 'testing' });
    try {
      const req: TestConnectRequest = {
        host: host.trim(),
        port,
        user: user.trim(),
        authType:
          kind === 'publicKey'
            ? 'publickey'
            : kind === 'keyboardInteractive'
              ? 'keyboard-interactive'
              : kind,
        jumpChain: [],
      };
      // 编辑已有会话且密码留空时，后端按 sessionId 回退保险库
      if (initial) req.sessionId = initial.id;
      if (kind === 'password' && password) req.password = password;
      if (kind === 'publicKey') {
        if (keyPath.trim()) req.keyPath = keyPath.trim();
        if (passphrase) req.passphrase = passphrase;
      }
      // 跳板链不再可编辑：测试沿用档案既有链（编辑时），新建无链
      if (initial && initial.jumpChain.length > 0) req.jumpChain = initial.jumpChain;
      const r = await invoke<TestConnectResult>('session_test_connect', { req });
      setTest(
        r.ok
          ? { phase: 'ok', latencyMs: r.latencyMs ?? null }
          : { phase: 'err', message: r.error ?? t('dialogs.connectFailed') },
      );
    } catch (e) {
      setTest({ phase: 'err', message: String(e) });
    }
  };

  /** connectAfter=false 时仅落库不发起连接（服务器暂不可达也应能保存） */
  const submit = async (connectAfter: boolean) => {
    setError(null);
    setBusy(true);
    try {
      if (sessKind === 'local') {
        // 本地会话：无认证/凭据；host/port/username/authType 存占位值（DB 列 NOT NULL）
        const id = initial?.id ?? `s-${Date.now()}`;
        const record: SessionRecord = {
          id,
          name: name.trim() || t('dialogs.localTerminal'),
          kind: 'local',
          host: 'localhost',
          port: 0,
          username: '',
          authType: 'password',
          keyPath: null,
          shell: shell || null,
          workdir: workdir.trim() || null,
          jumpChain: [],
          groupPath: presetGroup ?? initial?.groupPath ?? '',
          encoding,
          mcpPerms,
          mcpLevel,
          mcpProduction,
          tags: initial?.tags ?? [],
          command: command.trim() || null,
          createdAt: initial?.createdAt ?? '',
          updatedAt: '',
        };
        await invoke('session_upsert', { record });
        await loadSessions();
        connectBySession(id, record.name);
        return;
      }
      if (!save) {
        // 临时连接（不落库）：认证材料现取，公钥需读文件
        let auth: AuthSpec;
        if (kind === 'password') auth = { type: 'password', password };
        else if (kind === 'agent') auth = { type: 'agent' };
        else if (kind === 'keyboardInteractive') auth = { type: 'keyboardInteractive' };
        else {
          if (!keyPath.trim()) throw new Error(t('dialogs.keyPathRequired'));
          const keyPem = await invoke<string>('read_private_key', { path: keyPath.trim() });
          auth = { type: 'publicKey', keyPem, passphrase: passphrase || null };
        }
        connect({ host: host.trim(), port, user: user.trim(), auth, encoding, jumpChain: [] });
        return;
      }

      // 保存档案：秘密经 cred_set 进保险库（不经 sessions 表）
      const id = initial?.id ?? `s-${Date.now()}`;
      const record: SessionRecord = {
        id,
        kind: 'ssh',
        name: name.trim() || `${user.trim()}@${host.trim()}`,
        host: host.trim(),
        port,
        username: user.trim(),
        authType:
          kind === 'publicKey'
            ? 'publickey'
            : kind === 'keyboardInteractive'
              ? 'keyboard-interactive'
              : kind,
        keyPath: kind === 'publicKey' ? keyPath.trim() : null,
        // 跳板链编辑入口已移除：保存时原样保留档案既有链（新建为空）
        jumpChain: initial?.jumpChain ?? [],
        // 分组不再表单编辑：静默保留 preset（分组菜单新建）或原值
        groupPath: presetGroup ?? initial?.groupPath ?? '',
        color,
        encoding,
        // su 二级登录：空 = 不启用；密码走 cred_set 进保险库
        suUser: suUser.trim() || null,
        loginMacro: loginMacro.trim() ? loginMacro : null,
        mcpPerms,
        mcpLevel,
        mcpProduction,
        tags: initial?.tags ?? [],
        createdAt: initial?.createdAt ?? '',
        updatedAt: '',
      };
      // 保存路径不读私钥、不要求可达：认证在真正连接时才解析
      await invoke('session_upsert', { record });
      if (kind === 'password' && password)
        await invoke('cred_set', { sessionId: id, kind: 'password', secret: password });
      if (kind === 'publicKey' && passphrase)
        await invoke('cred_set', { sessionId: id, kind: 'keyPassphrase', secret: passphrase });
      // su 密码：配了才写；用户清空 suUser 时清掉保险库里的 su 密码（防残留生效）
      if (suUser.trim() && suPassword)
        await invoke('cred_set', { sessionId: id, kind: 'suPassword', secret: suPassword });
      if (!suUser.trim() && initial?.suUser)
        await invoke('cred_delete', { sessionId: id, kind: 'suPassword' });
      await loadSessions();
      if (connectAfter) connectBySession(id, record.name);
      else close();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const sshReady = host.trim() !== '' && user.trim() !== '' && portOk;
  const canSubmit = sessKind === 'local' || sshReady;
  const notReadyHint = sessKind === 'ssh' && !sshReady ? t('dialogs.fillRequired') : undefined;
  const advancedHasValue = suUser.trim() !== '' || loginMacro.trim() !== '' || encoding !== 'utf-8';

  const pickKeyFile = async () => {
    try {
      const picked = await open({ multiple: false, directory: false });
      if (typeof picked === 'string') setKeyPath(picked);
    } catch (e) {
      notify(t('dialogs.browseFailed', { error: String(e) }), 'error');
    }
  };

  const title = initial
    ? t('dialogs.titleEdit')
    : sessKind === 'local'
      ? t('dialogs.titleNewLocal')
      : t('dialogs.titleNewSsh');
  const ghostBtn =
    'h-9 rounded-md px-4 text-sm text-neutral-300 hover:bg-neutral-800 disabled:opacity-50';
  const outlineBtn =
    'h-9 rounded-md border border-neutral-700 px-4 text-sm text-neutral-200 hover:bg-neutral-800 disabled:opacity-50';

  return (
    <Dialog
      title={title}
      onClose={close}
      closeOnBackdrop={false}
      backdropClass="z-10"
      panelClass="myssh-tool flex max-h-[90vh] w-[40rem] max-w-[94vw] flex-col overflow-hidden rounded-xl border border-neutral-700 bg-neutral-900 text-sm text-neutral-200 shadow-xl"
    >
      <div className="flex items-center justify-between px-6 pt-5 pb-3">
        <h2 className="text-lg font-semibold text-neutral-100">{title}</h2>
        {/* 会话类型（批次十四）：本地终端 = 本机 PTY，适合跑 AI agent 等命令行工具；创建后锁定 */}
        {!initial && (
          <div className="inline-flex rounded-lg bg-neutral-800 p-1" role="group">
            {(
              [
                ['ssh', 'dialogs.kindSsh'],
                ['local', 'dialogs.localTerminal'],
              ] as const
            ).map(([k, label]) => (
              <button
                key={k}
                type="button"
                aria-pressed={sessKind === k}
                className={`h-8 rounded-md px-4 ${
                  sessKind === k
                    ? 'bg-blue-600 text-white'
                    : 'text-neutral-400 hover:text-neutral-100'
                }`}
                onClick={() => {
                  setSessKind(k);
                  setTab('basic');
                  if (k === 'local') {
                    if (!initial && !nameTouched) setName('');
                  } else {
                    followName(user, host);
                  }
                }}
              >
                {t(label)}
              </button>
            ))}
          </div>
        )}
      </div>

      {/* §9.1 双入口之一：服务器编辑器标签页 */}
      <div className="flex gap-1 border-b border-neutral-800 px-6" role="tablist">
        {(Object.keys(TAB_LABEL) as EditorTab[])
          .filter((tabKey) => sessKind === 'ssh' || tabKey === 'basic')
          .map((tabKey) => (
            <button
              key={tabKey}
              role="tab"
              aria-selected={tab === tabKey}
              className={`h-10 border-b-2 px-4 ${
                tab === tabKey
                  ? 'border-blue-500 text-neutral-100'
                  : 'border-transparent text-neutral-400 hover:text-neutral-200'
              }`}
              onClick={() => setTab(tabKey)}
            >
              {t(TAB_LABEL[tabKey])}
            </button>
          ))}
      </div>

      <div
        className="flex min-h-72 flex-1 flex-col gap-4 overflow-y-auto px-6 py-5"
        onKeyDown={(e) => {
          // 表单没有 <form>：输入框里按 Enter = 主操作（保存并连接/连接）；Dialog 基座不劫持输入控件的 Enter
          if (e.key !== 'Enter' || tab !== 'basic' || busy || !canSubmit) return;
          const el = e.target;
          if (el instanceof HTMLInputElement && el.type !== 'checkbox' && el.type !== 'color') {
            e.preventDefault();
            void submit(true);
          }
        }}
      >
        {tab === 'basic' && (
          <>
            {sessKind === 'ssh' && (
              <>
                <div className="flex gap-3">
                  <Field label={t('dialogs.host')} required className="flex-1">
                    <input
                      className={inputCls}
                      value={host}
                      autoComplete="off"
                      spellCheck={false}
                      placeholder={t('dialogs.hostPlaceholder')}
                      onChange={(e) => {
                        setHost(e.target.value);
                        followName(user, e.target.value);
                      }}
                      autoFocus
                    />
                  </Field>
                  <Field
                    label={t('dialogs.port')}
                    required
                    className="w-28"
                    error={portOk ? undefined : t('dialogs.portInvalid')}
                  >
                    <input
                      className={`${inputCls} ${portOk ? '' : 'border-red-600'}`}
                      inputMode="numeric"
                      value={portText}
                      onChange={(e) => setPortText(e.target.value.replace(/\D/g, '').slice(0, 5))}
                    />
                  </Field>
                </div>
                <Field label={t('dialogs.username')} required>
                  <input
                    className={inputCls}
                    value={user}
                    autoComplete="username"
                    spellCheck={false}
                    onChange={(e) => {
                      setUser(e.target.value);
                      followName(e.target.value, host);
                    }}
                  />
                </Field>

                <div className="border-t border-neutral-800" />

                <Field label={t('dialogs.authMethod')}>
                  <select
                    className={inputCls}
                    value={kind}
                    onChange={(e) => setKind(e.target.value as AuthKind)}
                  >
                    <option value="password">{t('dialogs.password')}</option>
                    <option value="publicKey">{t('dialogs.authPublicKey')}</option>
                    <option value="keyboardInteractive">{t('dialogs.authKi')}</option>
                    <option value="agent">ssh-agent / Pageant</option>
                  </select>
                </Field>
                {kind === 'password' && (
                  <Field label={initial ? t('dialogs.passwordLabelEdit') : t('dialogs.password')}>
                    <PasswordInput
                      value={password}
                      onChange={setPassword}
                      autoComplete={initial ? 'current-password' : 'new-password'}
                    />
                  </Field>
                )}
                {kind === 'publicKey' && (
                  <>
                    <Field label={t('dialogs.keyPath')}>
                      <div className="flex gap-2">
                        <input
                          className={inputCls}
                          value={keyPath}
                          autoComplete="off"
                          spellCheck={false}
                          onChange={(e) => setKeyPath(e.target.value)}
                          placeholder={t('dialogs.keyPathPlaceholder')}
                        />
                        <button
                          type="button"
                          className={`${outlineBtn} shrink-0`}
                          onClick={() => void pickKeyFile()}
                        >
                          {t('dialogs.browse')}
                        </button>
                      </div>
                    </Field>
                    <Field label={t('dialogs.passphraseLabel')}>
                      <PasswordInput value={passphrase} onChange={setPassphrase} />
                    </Field>
                  </>
                )}
              </>
            )}

            {sessKind === 'local' && (
              <>
                <Field label={t('dialogs.workdirLabel')}>
                  <input
                    className={inputCls}
                    value={workdir}
                    spellCheck={false}
                    onChange={(e) => setWorkdir(e.target.value)}
                    placeholder="D:\projects\my-app"
                    autoFocus
                  />
                </Field>
                <div className="flex gap-3">
                  <Field label="Shell" className="w-56 shrink-0">
                    <select
                      className={inputCls}
                      value={shell}
                      onChange={(e) => setShell(e.target.value)}
                    >
                      <option value="">{t('dialogs.shellAuto')}</option>
                      <option value="pwsh">{t('dialogs.shellPwsh')}</option>
                      <option value="powershell">Windows PowerShell</option>
                      <option value="cmd">{t('dialogs.shellCmd')}</option>
                    </select>
                  </Field>
                  <Field label={t('dialogs.commandLabel')} className="flex-1">
                    <input
                      className={inputCls}
                      value={command}
                      spellCheck={false}
                      onChange={(e) => setCommand(e.target.value)}
                      placeholder={t('dialogs.commandPlaceholder')}
                    />
                  </Field>
                </div>
                <p className="text-xs text-neutral-400">{t('dialogs.localNote')}</p>
              </>
            )}

            {(save || sessKind === 'local') && (
              <>
                <div className="border-t border-neutral-800" />
                <Field label={t('dialogs.labelName')}>
                  <input
                    className={inputCls}
                    value={name}
                    onChange={(e) => {
                      setNameTouched(true);
                      setName(e.target.value);
                    }}
                    placeholder={t('dialogs.namePlaceholder')}
                  />
                </Field>
                <div>
                  <span className="mb-1.5 block text-xs font-medium text-neutral-400">
                    {t('dialogs.labelColor')}
                  </span>
                  <div className="flex items-center gap-2">
                    <button
                      type="button"
                      title={t('dialogs.colorNone')}
                      aria-label={t('dialogs.colorNoneAria')}
                      className={`flex h-6 w-6 items-center justify-center rounded-full border border-neutral-600 text-xs leading-none text-neutral-400 ${
                        color === null ? SWATCH_ACTIVE : ''
                      }`}
                      onClick={() => setColor(null)}
                    >
                      ∅
                    </button>
                    {PRESET_COLORS.map((c) => (
                      <button
                        key={c}
                        type="button"
                        title={c}
                        aria-label={t('dialogs.colorValueAria', { color: c })}
                        style={{ backgroundColor: c }}
                        className={`h-6 w-6 rounded-full ${color === c ? SWATCH_ACTIVE : ''}`}
                        onClick={() => setColor(c)}
                      />
                    ))}
                    {/* 调色板任意颜色 */}
                    <input
                      type="color"
                      title={t('dialogs.customColor')}
                      aria-label={t('dialogs.customColor')}
                      value={color ?? '#3e63dd'}
                      onChange={(e) => setColor(e.target.value)}
                      className={`h-6 w-8 cursor-pointer rounded border border-neutral-600 bg-transparent p-0 ${
                        color !== null && !PRESET_COLORS.includes(color) ? SWATCH_ACTIVE : ''
                      }`}
                    />
                  </div>
                </div>
              </>
            )}

            {/* 高级选项折叠：su 二级登录 / 登录宏 / 终端编码；已配置过则默认展开 */}
            <details
              className="group rounded-lg border border-neutral-800"
              open={initial !== null && advancedHasValue}
            >
              <summary className="flex h-10 cursor-pointer list-none items-center gap-2 px-3 text-neutral-300 select-none hover:text-neutral-100">
                <Icon
                  name="chevronRight"
                  size={16}
                  className="transition-transform group-open:rotate-90"
                />
                {t('dialogs.advancedOptions')}
                {advancedHasValue && <span className="h-1.5 w-1.5 rounded-full bg-blue-400" />}
              </summary>
              <div className="flex flex-col gap-4 border-t border-neutral-800 p-3">
                {/* su 二级登录（批次二十二）：用普通用户登入后自动 su 到目标用户（如 root）。
                    密码仅存保险库；留空密码 = su 后手输 */}
                {sessKind === 'ssh' && (
                  <div>
                    <span className="mb-2 block font-medium text-neutral-300">
                      {t('dialogs.suSection')}
                    </span>
                    <div className="flex gap-3">
                      <Field label={t('dialogs.suUser')} className="flex-1">
                        <input
                          className={inputCls}
                          value={suUser}
                          autoComplete="off"
                          spellCheck={false}
                          placeholder={t('dialogs.suUserPlaceholder')}
                          onChange={(e) => setSuUser(e.target.value)}
                        />
                      </Field>
                      <Field label={t('dialogs.suPassword')} className="flex-1">
                        <PasswordInput
                          value={suPassword}
                          onChange={setSuPassword}
                          autoComplete="new-password"
                          placeholder={t('dialogs.suPasswordPlaceholder')}
                        />
                      </Field>
                    </div>
                  </div>
                )}
                {/* 登录宏：进 shell 后逐行自动执行（SSH/本地通用；
                    su 手输密码时后端跳过并 toast 告知） */}
                <Field label={t('dialogs.loginMacro')}>
                  <textarea
                    className={`${inputCls} h-24 resize-y py-2 font-mono`}
                    value={loginMacro}
                    spellCheck={false}
                    placeholder={t('dialogs.loginMacroPlaceholder')}
                    onChange={(e) => setLoginMacro(e.target.value)}
                  />
                </Field>
                {/* 终端编码：仅 SSH 会话可选；本地 ConPTY 协议面恒为 UTF-8，转码会出乱码 */}
                {sessKind === 'ssh' && (
                  <Field label={t('dialogs.encodingLabel')} className="w-56">
                    <select
                      className={inputCls}
                      value={encoding}
                      onChange={(e) => setEncoding(e.target.value)}
                    >
                      {ENCODING_OPTIONS.map(([value, label]) => (
                        <option key={value} value={value}>
                          {t(label)}
                        </option>
                      ))}
                    </select>
                  </Field>
                )}
              </div>
            </details>

            {sessKind === 'ssh' && (
              <label className="flex cursor-pointer items-center gap-2.5 text-neutral-300">
                <input
                  type="checkbox"
                  className="size-4"
                  checked={save}
                  onChange={(e) => setSave(e.target.checked)}
                  disabled={initial !== null}
                />
                {t('dialogs.saveSessionLabel')}
              </label>
            )}
          </>
        )}

        {tab === 'tunnels' &&
          (initial ? (
            <SessionTunnelsTab sessionId={initial.id} />
          ) : (
            <p className="py-8 text-center text-neutral-400">{t('dialogs.tunnelsNeedSave')}</p>
          ))}

        {tab === 'mcp' && (
          <div>
            <p className="mb-3 text-neutral-400">{t('dialogs.mcpPermHint')}</p>
            <div className="space-y-3">
              <div className="flex items-center justify-between gap-3">
                <span className="text-neutral-200">{t('dialogs.mcpLevel')}</span>
                <select
                  className={`${inputCls} w-56`}
                  value={mcpLevel ?? 'inherit'}
                  onChange={(e) =>
                    setMcpLevel(e.target.value === 'inherit' ? null : e.target.value)
                  }
                >
                  <option value="inherit">{t('dialogs.mcpPermInherit')}</option>
                  {MCP_LEVELS.map(([v, k]) => (
                    <option key={v} value={v}>
                      {t(k)}
                    </option>
                  ))}
                </select>
              </div>
              <div className="flex items-center justify-between gap-3">
                <span className="text-neutral-200">{t('dialogs.mcpProduction')}</span>
                <select
                  className={`${inputCls} w-56`}
                  value={mcpProduction === null ? 'inherit' : mcpProduction ? 'yes' : 'no'}
                  onChange={(e) =>
                    setMcpProduction(e.target.value === 'inherit' ? null : e.target.value === 'yes')
                  }
                >
                  <option value="inherit">{t('dialogs.mcpPermInherit')}</option>
                  <option value="yes">{t('dialogs.mcpProductionYes')}</option>
                  <option value="no">{t('dialogs.mcpProductionNo')}</option>
                </select>
              </div>
              {MCP_PERM_GROUPS.map(([group, labelKey]) => {
                const v = mcpPerms[group];
                return (
                  <div key={group} className="flex items-center justify-between gap-3">
                    <span className="text-neutral-200">{t(labelKey)}</span>
                    <select
                      className={`${inputCls} w-40`}
                      value={v === undefined ? 'inherit' : v ? 'allow' : 'deny'}
                      onChange={(e) => {
                        const mode = e.target.value;
                        // 继承 = 键缺席（稀疏映射）
                        setMcpPerms((prev) => setPermMode(prev, group, mode));
                      }}
                    >
                      <option value="inherit">{t('dialogs.mcpPermInherit')}</option>
                      <option value="allow">{t('dialogs.mcpPermAllow')}</option>
                      <option value="deny">{t('dialogs.mcpPermDeny')}</option>
                    </select>
                  </div>
                );
              })}
            </div>
          </div>
        )}

        {error && (
          <p aria-live="polite" className="text-red-400">
            {error}
          </p>
        )}
      </div>

      <div className="flex items-center justify-between gap-3 border-t border-neutral-800 px-6 py-4">
        <div className="flex min-w-0 items-center gap-3">
          {sessKind === 'ssh' && (
            <button
              className={`${outlineBtn} shrink-0`}
              onClick={() => void testConnect()}
              disabled={busy || test.phase === 'testing' || !sshReady}
              title={notReadyHint}
            >
              {test.phase === 'testing' ? t('dialogs.testing') : t('dialogs.testConnect')}
            </button>
          )}
          {test.phase === 'ok' && (
            <span aria-live="polite" className="flex items-center gap-1.5 truncate text-green-400">
              <Icon name="checkCircle" size={16} />
              {test.latencyMs !== null
                ? t('dialogs.testOkLatency', { ms: test.latencyMs })
                : t('dialogs.testOk')}
            </span>
          )}
          {test.phase === 'err' && (
            <span
              aria-live="polite"
              className="flex min-w-0 items-start gap-1.5 text-red-400"
              title={test.message}
            >
              <Icon name="alert" size={16} className="mt-0.5 shrink-0" />
              <span className="line-clamp-2 break-all">{test.message}</span>
            </span>
          )}
        </div>
        <div className="flex shrink-0 gap-2">
          <button className={ghostBtn} onClick={close}>
            {t('dialogs.cancel')}
          </button>
          {sessKind === 'ssh' && save && (
            <button
              className={outlineBtn}
              onClick={() => void submit(false)}
              disabled={busy || !sshReady}
              title={notReadyHint}
            >
              {t('dialogs.saveOnly')}
            </button>
          )}
          <button
            className="h-9 rounded-md bg-blue-600 px-5 text-sm font-medium text-white hover:bg-blue-500 disabled:opacity-50"
            onClick={() => void submit(true)}
            disabled={busy || !canSubmit}
            title={notReadyHint}
          >
            {sessKind === 'local'
              ? t('dialogs.saveAndOpen')
              : save
                ? t('dialogs.saveAndConnect')
                : t('dialogs.connect')}
          </button>
        </div>
      </div>
    </Dialog>
  );
}
/** 隧道页（§9.1）：只列绑定当前服务器的规则；启动/停止/编辑/复制/删除 + 新建。 */
function SessionTunnelsTab({ sessionId }: { sessionId: string }) {
  const tunnelDefs = useAppStore((s) => s.tunnelDefs);
  const tunnels = useAppStore((s) => s.tunnels);
  const stopTunnel = useAppStore((s) => s.stopTunnel);
  const saveTunnel = useAppStore((s) => s.saveTunnel);
  const deleteTunnel = useAppStore((s) => s.deleteTunnel);
  const duplicateTunnel = useAppStore((s) => s.duplicateTunnel);
  const loadTunnelDefs = useAppStore((s) => s.loadTunnelDefs);
  const notify = useAppStore((s) => s.notify);
  const t = useT();

  const [editor, setEditor] = useState<{ def: TunnelDef | null } | undefined>(undefined);
  const [pendingDelete, setPendingDelete] = useState<TunnelDef | null>(null);

  // 面板外打开时定义可能未加载
  useEffect(() => {
    void loadTunnelDefs();
  }, [loadTunnelDefs]);

  const defs = tunnelDefs.filter((d) => d.sessionId === sessionId);
  const runtimeById = new Map(tunnels.map((t) => [t.tunnelId, t]));

  const startDef = async (d: TunnelDef) => {
    try {
      await saveTunnel(d, true);
    } catch (e) {
      notify(t('dialogs.tunnelStartFailed', { error: String(e) }), 'error');
    }
  };

  const kindLabel = (kind: string) => {
    const k = TUNNEL_KIND_KEY[kind];
    return k ? t(k) : kind;
  };

  return (
    <div className="text-xs">
      <div className="mb-1 flex items-center justify-between">
        <span className="text-neutral-500">{t('dialogs.tunnelsBoundOnly')}</span>
        <button
          className="rounded border border-neutral-700 px-2 py-0.5 hover:bg-neutral-800"
          onClick={() => setEditor({ def: null })}
        >
          {t('dialogs.addTunnel')}
        </button>
      </div>
      {defs.length === 0 ? (
        <p className="py-4 text-center text-neutral-600">{t('dialogs.noTunnels')}</p>
      ) : (
        <ul>
          {defs.map((d) => {
            const rt = runtimeById.get(d.id);
            const statusKey = rt ? TUNNEL_STATUS_KEY[rt.status] : undefined;
            return (
              <li key={d.id} className="mb-1 rounded border border-neutral-800 px-2 py-1.5">
                {/* 第一行：名称在前（主体），类型徽标 + 状态，操作按钮右对齐 */}
                <div className="flex items-center gap-2">
                  <span
                    className="min-w-0 flex-1 truncate font-medium text-neutral-200"
                    title={tunnelDisplayName(d)}
                  >
                    {tunnelDisplayName(d)}
                  </span>
                  <span className="shrink-0 rounded bg-neutral-800 px-1.5 py-px text-neutral-400">
                    {kindLabel(d.kind)}
                  </span>
                  <span
                    className={`shrink-0 ${
                      rt
                        ? rt.status === 'listening'
                          ? 'text-green-400'
                          : rt.status === 'failed'
                            ? 'text-red-400'
                            : 'text-yellow-400'
                        : 'text-neutral-600'
                    }`}
                    title={rt?.lastError ?? undefined}
                  >
                    {rt ? (statusKey ? t(statusKey) : rt.status) : t('dialogs.notRunning')}
                  </span>
                  {rt ? (
                    <button
                      className="shrink-0 rounded px-1 text-neutral-500 hover:text-red-400"
                      onClick={() => void stopTunnel(d.id)}
                    >
                      {t('dialogs.stop')}
                    </button>
                  ) : (
                    <button
                      className="shrink-0 rounded px-1 text-neutral-500 hover:text-green-400"
                      onClick={() => void startDef(d)}
                    >
                      {t('dialogs.start')}
                    </button>
                  )}
                  <button
                    className="shrink-0 rounded px-1 text-neutral-500 hover:text-neutral-200"
                    onClick={() => setEditor({ def: d })}
                  >
                    {t('dialogs.edit')}
                  </button>
                  <button
                    className="shrink-0 rounded px-1 text-neutral-500 hover:text-neutral-200"
                    onClick={() => void duplicateTunnel(d)}
                  >
                    {t('dialogs.duplicate')}
                  </button>
                  <button
                    className="shrink-0 rounded px-1 text-neutral-500 hover:text-red-400"
                    onClick={() => setPendingDelete(d)}
                  >
                    {t('dialogs.delete')}
                  </button>
                </div>
                {/* 第二行：地址映射全显（可换行不截断）+ 启动方式 */}
                <div className="mt-0.5 flex items-baseline gap-2">
                  <span className="min-w-0 flex-1 font-mono break-all text-neutral-400">
                    {d.bindHost}:{d.bindPort}
                    {d.targetHost ? ` → ${d.targetHost}:${d.targetPort}` : ''}
                  </span>
                  <span className="shrink-0 text-neutral-600">
                    {START_MODE_LABEL[startModeOf(d)]}
                  </span>
                </div>
              </li>
            );
          })}
        </ul>
      )}

      {editor && (
        <TunnelEditor
          sessionId={sessionId}
          initial={editor.def}
          running={editor.def ? runtimeById.has(editor.def.id) : false}
          onClose={() => setEditor(undefined)}
        />
      )}
      {pendingDelete && (
        <ConfirmDialog
          title={t('dialogs.deleteTunnelTitle', { name: tunnelDisplayName(pendingDelete) })}
          confirmLabel={t('dialogs.delete')}
          onConfirm={() => {
            void deleteTunnel(pendingDelete.id)
              .then(() => notify(t('dialogs.tunnelDeleted'), 'success'))
              .catch((e) => notify(t('dialogs.tunnelDeleteFailed', { error: String(e) }), 'error'));
            setPendingDelete(null);
          }}
          onCancel={() => setPendingDelete(null)}
        >
          {t('dialogs.deleteTunnelBody', {
            spec: `${pendingDelete.bindHost}:${pendingDelete.bindPort}${
              pendingDelete.targetHost
                ? ` → ${pendingDelete.targetHost}:${pendingDelete.targetPort}`
                : ''
            }`,
          })}
        </ConfirmDialog>
      )}
    </div>
  );
}
