const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const statusBand = document.querySelector('.status-band');
const statusEl = document.getElementById('status');
const runtimeEl = document.getElementById('runtime');
const logEl = document.getElementById('log');
const copyButton = document.getElementById('copy');
const restartButton = document.getElementById('restart');

function setStatus(status) {
  statusEl.textContent = status;
  statusBand.classList.toggle('error', /失败|错误|找不到|不支持|退出/.test(status));
  statusBand.classList.toggle('ready', /已就绪|正在打开/.test(status));
}

function appendLog(line) {
  logEl.textContent += line.endsWith('\n') ? line : line + '\n';
  logEl.scrollTop = logEl.scrollHeight;
}

async function initialize() {
  await listen('dsh-log', (event) => appendLog(event.payload));
  await listen('dsh-status', (event) => setStatus(event.payload));
  await listen('dsh-runtime', (event) => { runtimeEl.textContent = event.payload; });

  const snapshot = await invoke('get_snapshot');
  setStatus(snapshot.status);
  runtimeEl.textContent = snapshot.runtime || '正在检查本地运行环境';
  logEl.textContent = snapshot.logs.join('');
  logEl.scrollTop = logEl.scrollHeight;
}

copyButton.addEventListener('click', async () => {
  copyButton.disabled = true;
  try {
    const diagnostics = await invoke('get_diagnostics');
    await navigator.clipboard.writeText(diagnostics);
    copyButton.textContent = '已复制';
  } catch (error) {
    setStatus('复制诊断失败：' + error);
  } finally {
    window.setTimeout(() => {
      copyButton.textContent = '复制诊断';
      copyButton.disabled = false;
    }, 1200);
  }
});

restartButton.addEventListener('click', async () => {
  restartButton.disabled = true;
  try {
    await invoke('restart_dsh');
  } catch (error) {
    setStatus('重启失败：' + error);
  } finally {
    restartButton.disabled = false;
  }
});

initialize().catch((error) => setStatus('初始化失败：' + error));
