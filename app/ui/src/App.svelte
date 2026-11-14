<script>
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { getCurrentWebview } from '@tauri-apps/api/webview';
  import { open } from '@tauri-apps/plugin-dialog';

  let info = $state(null);
  let startupError = $state('');
  let peers = $state([]);
  let transfers = $state([]);
  let notice = $state('');

  // Pairing panel: null (closed), 'show' (this device shows a code) or 'enter'.
  let pairing = $state(null);
  let ticket = $state('');
  let ticketQr = $state('');
  let codeInput = $state('');
  let pairBusy = $state(false);
  let pairError = $state('');
  let copied = $state(false);

  // Id of the device card files are being dragged over.
  let dropTarget = $state(null);

  const PATH_LABEL = {
    lan: '로컬 네트워크',
    direct: '직접 연결',
    relay: '릴레이 경유',
    unknown: '연결 중',
  };

  onMount(() => {
    const unlisten = [];
    (async () => {
      try {
        info = await invoke('info');
        peers = await invoke('peers');
      } catch (e) {
        startupError = String(e);
        return;
      }
      unlisten.push(await listen('anytrans-event', (e) => onEvent(e.payload)));
      unlisten.push(await getCurrentWebview().onDragDropEvent((e) => onDragDrop(e.payload)));
    })();
    return () => unlisten.forEach((stop) => stop());
  });

  function onEvent(event) {
    if (event.type === 'paired') {
      refreshPeers();
      pairing = null;
      notice = `${event.peer.name} 기기와 연결되었습니다.`;
      return;
    }
    if (event.type === 'transfer_started') {
      const fresh = {
        id: event.id,
        peer: event.peer,
        direction: event.direction,
        files: event.files,
        total: event.total_bytes,
        done: event.resumed_bytes,
        resumed: event.resumed_bytes,
        path: 'unknown',
        status: 'active',
        error: '',
        saved: [],
      };
      transfers = [fresh, ...transfers.filter((t) => t.id !== event.id)];
      return;
    }
    const transfer = transfers.find((t) => t.id === event.id);
    if (!transfer) return;
    if (event.type === 'progress') {
      transfer.done = event.done_bytes;
      transfer.path = event.path;
    } else if (event.type === 'transfer_finished') {
      transfer.status = 'done';
      transfer.done = transfer.total;
      transfer.saved = event.saved;
    } else if (event.type === 'transfer_failed') {
      transfer.status = 'failed';
      transfer.error = event.error;
    }
  }

  async function refreshPeers() {
    peers = await invoke('peers');
  }

  function peerAt(position) {
    const scale = window.devicePixelRatio || 1;
    const el = document.elementFromPoint(position.x / scale, position.y / scale);
    return el?.closest('[data-peer]')?.dataset.peer ?? null;
  }

  function onDragDrop(payload) {
    if (payload.type === 'leave') {
      dropTarget = null;
      return;
    }
    // With a single device there is nothing to aim at: anywhere will do.
    const target = peerAt(payload.position) ?? (peers.length === 1 ? peers[0].id : null);
    if (payload.type === 'drop') {
      dropTarget = null;
      if (target && payload.paths.length > 0) {
        send(target, payload.paths);
      } else if (peers.length > 1) {
        notice = '보낼 기기 위에 놓아 주세요.';
      }
    } else {
      dropTarget = target;
    }
  }

  async function send(to, paths) {
    notice = '';
    try {
      await invoke('send', { to, paths });
    } catch (e) {
      // Failures after the transfer started already show in the transfer list.
      if (!transfers.some((t) => t.status === 'failed' && String(e).includes(t.error))) {
        notice = `보내지 못했습니다: ${e}`;
      }
    }
  }

  // `kind` is 'file', 'folder' or 'media' (the photo library, on phones).
  async function pick(to, kind) {
    const options = {
      file: { multiple: true },
      folder: { directory: true },
      media: { multiple: true, pickerMode: 'media' },
    }[kind];
    const picked = await open(options);
    if (!picked) return;
    send(to, Array.isArray(picked) ? picked : [picked]);
  }

  async function forget(peer) {
    await invoke('forget', { id: peer.id });
    refreshPeers();
  }

  async function showCode() {
    pairing = 'show';
    pairError = '';
    ticket = '';
    ticketQr = '';
    copied = false;
    try {
      const created = await invoke('pairing_ticket');
      ticket = created.code;
      ticketQr = created.qr;
    } catch (e) {
      pairError = String(e);
    }
  }

  function enterCode() {
    pairing = 'enter';
    pairError = '';
    codeInput = '';
  }

  async function submitCode() {
    pairBusy = true;
    pairError = '';
    try {
      await invoke('pair', { code: codeInput });
    } catch (e) {
      pairError = String(e);
    } finally {
      pairBusy = false;
    }
  }

  async function scanCode() {
    pairError = '';
    try {
      const scanner = await import('@tauri-apps/plugin-barcode-scanner');
      let permission = await scanner.checkPermissions();
      if (permission !== 'granted') permission = await scanner.requestPermissions();
      if (permission !== 'granted') {
        pairError = '카메라 권한이 필요합니다. 설정 앱에서 허용해 주세요.';
        return;
      }
      const result = await scanner.scan({ windowed: false, formats: [scanner.Format.QRCode] });
      codeInput = result.content;
      await submitCode();
    } catch (e) {
      pairError = String(e);
    }
  }

  async function copyTicket() {
    await navigator.clipboard.writeText(ticket);
    copied = true;
  }

  function human(bytes) {
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    let value = bytes;
    let unit = 0;
    while (value >= 1024 && unit < units.length - 1) {
      value /= 1024;
      unit += 1;
    }
    return unit === 0 ? `${bytes} B` : `${value.toFixed(1)} ${units[unit]}`;
  }

  function percent(t) {
    return t.total === 0 ? 100 : Math.floor((t.done * 100) / t.total);
  }
</script>

{#if startupError}
  <main class="fatal">
    <h1>AnyTrans를 시작할 수 없습니다</h1>
    <p>{startupError}</p>
  </main>
{:else if info}
  <main>
    <header>
      <div>
        <h1>{info.name}</h1>
        <span class="muted">이 기기</span>
      </div>
      <button onclick={() => invoke('open_downloads')}>받은 파일</button>
    </header>

    {#if notice}
      <button class="notice" onclick={() => (notice = '')}>{notice}</button>
    {/if}

    <section>
      <div class="section-head">
        <h2>내 기기</h2>
        {#if !pairing}
          <button class="link" onclick={showCode}>기기 추가</button>
        {/if}
      </div>

      {#if pairing}
        <div class="card pairing">
          <div class="tabs">
            <button class:active={pairing === 'show'} onclick={showCode}>코드 보여주기</button>
            <button class:active={pairing === 'enter'} onclick={enterCode}>코드 입력하기</button>
          </div>
          {#if pairing === 'show'}
            <p class="muted">
              다른 기기에서 QR을 스캔하거나 코드를 입력하세요. 5분 동안, 한 번만 쓸 수 있습니다.
            </p>
            {#if ticketQr}
              <!-- SVG built by our own backend from the pairing code. -->
              <div class="qr">{@html ticketQr}</div>
            {/if}
            <textarea rows="4" readonly value={ticket} placeholder="코드를 만드는 중…"></textarea>
            <div class="row">
              <button class="primary" disabled={!ticket} onclick={copyTicket}>
                {copied ? '복사됨' : '코드 복사'}
              </button>
              <button onclick={() => (pairing = null)}>닫기</button>
            </div>
          {:else}
            <p class="muted">
              {info.mobile ? '다른 기기에 표시된 QR을 스캔하거나 코드를 붙여 넣으세요.' : '다른 기기에 표시된 코드를 붙여 넣으세요.'}
            </p>
            {#if info.mobile}
              <button class="primary" disabled={pairBusy} onclick={scanCode}>QR 스캔</button>
            {/if}
            <textarea rows="5" bind:value={codeInput} placeholder="코드 붙여넣기"></textarea>
            <div class="row">
              <button class="primary" disabled={pairBusy || !codeInput.trim()} onclick={submitCode}>
                {pairBusy ? '연결 중…' : '연결'}
              </button>
              <button onclick={() => (pairing = null)}>닫기</button>
            </div>
          {/if}
          {#if pairError}<p class="error">{pairError}</p>{/if}
        </div>
      {/if}

      {#each peers as peer (peer.id)}
        <div class="card device" class:drop={dropTarget === peer.id} data-peer={peer.id}>
          <div class="device-name">
            <strong>{peer.name}</strong>
            <span class="muted">{peer.id.slice(0, 10)}</span>
          </div>
          <div class="row">
            <button class="primary" onclick={() => pick(peer.id, 'file')}>파일 보내기</button>
            {#if info.mobile}
              <button onclick={() => pick(peer.id, 'media')}>사진 보내기</button>
            {:else}
              <button onclick={() => pick(peer.id, 'folder')}>폴더 보내기</button>
            {/if}
            <button class="link forget" onclick={() => forget(peer)}>삭제</button>
          </div>
        </div>
      {:else}
        {#if !pairing}
          <div class="card empty">
            <p>아직 연결된 기기가 없습니다.</p>
            <button class="primary" onclick={showCode}>기기 추가</button>
          </div>
        {/if}
      {/each}
      {#if peers.length > 0 && !info.mobile}
        <p class="hint muted">파일이나 폴더를 기기 위로 끌어다 놓아도 됩니다.</p>
      {/if}
    </section>

    {#if transfers.length > 0}
      <section>
        <div class="section-head"><h2>전송</h2></div>
        {#each transfers as t (t.id)}
          <div class="card transfer">
            <div class="transfer-head">
              <strong>{t.direction === 'send' ? '보내기' : '받기'} · {t.peer}</strong>
              <span class="muted">파일 {t.files}개 · {human(t.total)}</span>
            </div>
            <div class="track" role="progressbar" aria-valuenow={percent(t)} aria-valuemin="0" aria-valuemax="100">
              <div class="fill {t.status}" style="width: {percent(t)}%"></div>
            </div>
            <div class="transfer-foot">
              {#if t.status === 'active'}
                <span>{percent(t)}% · {human(t.done)}</span>
                <span class="muted">
                  {PATH_LABEL[t.path]}{t.resumed > 0 ? ' · 이어받기' : ''}
                </span>
              {:else if t.status === 'done'}
                <span class="ok">완료</span>
                {#if t.saved.length > 0}
                  <button class="link" onclick={() => invoke('open_downloads')}>폴더 열기</button>
                {/if}
              {:else}
                <span class="error">실패: {t.error}</span>
              {/if}
            </div>
          </div>
        {/each}
      </section>
    {/if}
  </main>
{/if}

<style>
  main {
    max-width: 560px;
    margin: 0 auto;
    /* Keep clear of the notch and the home indicator on phones. */
    padding: calc(16px + env(safe-area-inset-top)) calc(16px + env(safe-area-inset-right))
      calc(16px + env(safe-area-inset-bottom)) calc(16px + env(safe-area-inset-left));
    display: flex;
    flex-direction: column;
    gap: 20px;
  }

  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }

  h1 {
    margin: 0;
    font-size: 18px;
    overflow-wrap: anywhere;
  }

  h2 {
    margin: 0;
    font-size: 12px;
    font-weight: 600;
    color: var(--muted);
    letter-spacing: 0.04em;
  }

  section {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .section-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
  }

  .card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 12px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .device.drop {
    border-color: var(--accent);
    background: var(--accent-soft);
  }

  .device-name {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 8px;
  }

  .device-name strong {
    overflow-wrap: anywhere;
  }

  .row {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: center;
  }

  .forget {
    margin-left: auto;
    color: var(--muted);
  }

  .empty {
    align-items: center;
    padding: 24px 12px;
  }

  .empty p,
  .pairing p {
    margin: 0;
  }

  .qr {
    align-self: center;
    line-height: 0;
    padding: 8px;
    border-radius: 8px;
    background: #ffffff;
  }

  .qr :global(svg) {
    width: 200px;
    height: 200px;
  }

  .tabs {
    display: flex;
    gap: 6px;
  }

  .tabs button {
    flex: 1;
  }

  .tabs button.active {
    background: var(--accent-soft);
    border-color: var(--accent);
    font-weight: 600;
  }

  .hint {
    margin: 0;
    font-size: 12px;
    text-align: center;
  }

  .transfer-head,
  .transfer-foot {
    display: flex;
    justify-content: space-between;
    gap: 8px;
    font-size: 13px;
  }

  .transfer-head strong,
  .transfer-foot span {
    overflow-wrap: anywhere;
    min-width: 0;
  }

  .track {
    height: 6px;
    border-radius: 3px;
    background: var(--track);
    overflow: hidden;
  }

  .fill {
    height: 100%;
    background: var(--accent);
    transition: width 0.15s linear;
  }

  .fill.done {
    background: var(--ok);
  }

  .fill.failed {
    background: var(--danger);
  }

  .notice {
    text-align: left;
    background: var(--accent-soft);
    border-color: var(--accent);
  }

  .muted {
    color: var(--muted);
    font-size: 12px;
  }

  .ok {
    color: var(--ok);
  }

  .error {
    color: var(--danger);
    margin: 0;
  }

  .fatal {
    padding: 32px 20px;
    -webkit-user-select: text;
    user-select: text;
  }
</style>
