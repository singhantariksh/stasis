<script lang="ts">
  let { code, language = '' } = $props<{ code: string; language?: string }>();
  
  let copied = $state(false);
  
  function copyCode() {
    navigator.clipboard.writeText(code).then(() => {
      copied = true;
      setTimeout(() => {
        copied = false;
      }, 2000);
    });
  }
</script>

<div class="code-block">
  <button class="copy-button" onclick={copyCode} aria-label="Copy code">
    {#if copied}
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
        <polyline points="20 6 9 17 4 12"></polyline>
      </svg>
      <span>Copied!</span>
    {:else}
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
        <rect x="9" y="9" width="13" height="13" rx="2" ry="2"></rect>
        <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"></path>
      </svg>
      <span>Copy</span>
    {/if}
  </button>
  <pre><code class={language}>{code}</code></pre>
</div>

<style>
  .code-block {
    position: relative;
    margin: 16px 0;
  }
  
  .copy-button {
    position: absolute;
    top: 27px;
    transform: translateY(-50%);
    right: 12px;
    display: flex;
    gap: 6px;
    background: var(--bg-primary);
    border: 1px solid var(--border-color);
    color: var(--text-secondary);
    padding: 6px 12px;
    border-radius: 6px;
    font-size: 0.85rem;
    cursor: pointer;
    transition: all 0.2s ease;
    z-index: 10;
  }
  
  .copy-button:hover {
    background: var(--bg-secondary);
    color: var(--text-primary);
    border-color: var(--accent);
  }
  
  .copy-button svg {
    flex-shrink: 0;
  }
  
  pre {
    background: var(--bg-secondary);
    padding: 16px;
    padding-right: 100px; /* Make room for copy button */
    border-radius: 6px;
    overflow-x: auto;
    border: 1px solid var(--border-color);
    margin: 0;
    min-height: 48px; /* Ensure consistent minimum height */
    display: flex;
    align-items: center; /* Center single-line code vertically */
  }
  
  pre code {
    background: none;
    padding: 0;
    font-size: 0.9rem;
    font-family: 'Consolas', 'Monaco', 'Courier New', monospace;
    color: var(--text-primary);
    line-height: 1.5;
    width: 100%;
  }
  
  @media (max-width: 768px) {
    .copy-button span {
      display: none; /* Hide text on mobile, keep icon */
    }
    
    .copy-button {
      padding: 8px;
      top: 25px;
      right: 8px;
    }
    
    pre {
      padding: 12px;
      padding-right: 50px;
      font-size: 0.8rem;
    }
    
    pre code {
      font-size: 0.8rem;
    }
  }
</style>
