<script lang="ts">
  import { onMount } from 'svelte';
  import FileTree from './components/FileTree.svelte';
  import { ChevronRight, File, Folder, FolderOpen, Download, X, Plus, Search, Settings, HelpCircle, Menu, ChevronDown, Copy, Edit, Trash2, FilePlus, FolderPlus } from '@lucide/svelte';

  // Types
  interface FileItem {
    path: string;
    name: string;
    type: 'file' | 'folder';
    children?: FileItem[];
    content?: string;
    dirty?: boolean;
  }

  interface Tab {
    path: string;
    name: string;
    dirty: boolean;
  }

  type ViewMode = 'html' | 'json' | 'page' | 'pdf';

  // State
  let fileTree: FileItem[] = [];
  let tabs: Tab[] = [];
  let activeTab: string | null = null;
  let editorContent = "";

  $: if (activeTab) editorContent = fs.readFile(activeTab) || "";
  let viewMode: ViewMode = 'page';
  let autoRender = true;
  let autoLint = true;
  let previewContent = '';
  let status = 'Ready';
  let showContextMenu = false;
  let contextMenuPos = { x: 0, y: 0 };
  let contextMenuTarget: { path: string; type: 'file' | 'folder' } | null = null;
  let showModal = false;
  let modalTitle = '';
  let modalInput = '';
  let modalType = '';
  let modalData: any = {};
  let ymxInstance: any = null;
  let ironpressLoaded = false;
  let sidebarOpen = true;
  let dragSource: string | null = null;
  let dragOverTarget: string | null = null;

  // WASM initialization
  async function initWasm() {
    try {
      const wasmModule = await import('./pkg/ymx_web.js');
      ymxInstance = new wasmModule.Ymx();
      status = 'WASM loaded';
    } catch (err) {
      console.error('WASM init failed:', err);
      status = 'WASM failed to load';
    }
  }

  // Virtual filesystem
  const fs = {
    root: { type: 'folder' as const, children: {} as Record<string, any> },
    
    init() {
      this.createFile('/main.yml', `main:
  message: "Hello, YMX!"
  items:
    - "Apple"
    - "Banana"
    - "Cherry"

html_card:
  from: div
  class: "card"
  children:
    - from: h2
      children: "Hello, YMX!"
    - from: ul
      children:
        - from: li
          children: "Item 1"
        - from: li
          children: "Item 2"
    - from: p
      children: "Welcome to YMX Playground"`);
      
      this.createFile('/components.yml', `# Reusable components

button:
  from: button
  class: "btn btn-{{variant || 'primary'}}"
  children: "{{label}}"

card:
  from: div
  class: "card"
  children:
    - from: "{{headerTag || 'h3'}}"
      children: "{{title}}"
    - from: div
      class: "card-body"
      children: "{{content}}"`);
      
      this.createFolder('/examples');
      this.createFile('/examples/math.yml', `# Math expressions in YMX

calculator:
  a: 10
  b: 5
  sum: "\${a + b}"
  diff: "\${a - b}"
  product: "\${a * b}"
  quotient: "\${a / b}"
  power: "\${a ** b}"

formatter:
  name: "YMX"
  version: 1
  display: "\${upper(name)} v\${version}"`);
    },

    createFolder(path: string) {
      const parts = path.split('/').filter(Boolean);
      let current = this.root;
      let currentPath = '';
      
      for (const part of parts) {
        currentPath += '/' + part;
        if (!current.children[part]) {
          current.children[part] = { type: 'folder', children: {} };
        }
        current = current.children[part];
      }
    },

    createFile(path: string, content: string = '') {
      const parts = path.split('/').filter(Boolean);
      const fileName = parts.pop()!;
      const folderPath = '/' + parts.join('/');
      
      this.createFolder(folderPath);
      
      let current = this.root;
      for (const part of parts) {
        current = current.children[part];
      }
      
      current.children[fileName] = { type: 'file', content };
    },

    deleteItem(path: string) {
      const parts = path.split('/').filter(Boolean);
      const name = parts.pop()!;
      const folderPath = '/' + parts.join('/');
      
      let current = this.root;
      for (const part of parts) {
        current = current.children[part];
      }
      
      if (current.children[name]) {
        delete current.children[name];
      }
    },

    readFile(path: string): string | null {
      const parts = path.split('/').filter(Boolean);
      let current = this.root;
      for (const part of parts) {
        if (!current.children[part]) return null;
        current = current.children[part];
      }
      return current.type === 'file' ? current.content : null;
    },

    writeFile(path: string, content: string) {
      const parts = path.split('/').filter(Boolean);
      const fileName = parts.pop()!;
      
      let current = this.root;
      for (const part of parts) {
        current = current.children[part];
      }
      
      if (current.children[fileName] && current.children[fileName].type === 'file') {
        current.children[fileName].content = content;
      }
    },

    listItems(path: string) {
      const parts = path.split('/').filter(Boolean);
      let current = this.root;
      for (const part of parts) {
        if (!current.children[part]) return [];
        current = current.children[part];
      }
      return Object.entries(current.children).map(([name, item]) => ({
        name,
        path: path === '/' ? '/' + name : path + '/' + name,
        ...item
      }));
    },

    toTree(path: string = '/'): FileItem[] {
      const items = this.listItems(path);
      return items.map(item => ({
        ...item,
        children: item.type === 'folder' ? this.toTree(item.path) : undefined
      }));
    },

    moveItem(fromPath: string, toPath: string) {
      const fromParts = fromPath.split('/').filter(Boolean);
      const toParts = toPath.split('/').filter(Boolean);
      const name = fromParts.pop()!;
      
      // Prevent self-move
      if (toPath === fromPath) return false;
      
      // Prevent moving folder into itself
      if (fromPath.startsWith(toPath + '/')) return false;
      
      let fromParent = this.root;
      for (const part of fromParts) {
        fromParent = fromParent.children[part];
      }
      
      let toParent = this.root;
      for (const part of toParts) {
        toParent = toParent.children[part];
      }
      
      const item = fromParent.children[name];
      if (!item) return false;
      
      // Check conflicts
      if (toParent.children[name]) {
        return { conflict: true, type: item.type };
      }
      
      delete fromParent.children[name];
      toParent.children[name] = item;
      return true;
    }
  };

  // Convert flat tree to nested for FileTree component
  function buildFileTree(): FileItem[] {
    return fs.toTree('/');
  }

  // Tab management
  function openTab(path: string) {
    const existing = tabs.find(t => t.path === path);
    if (existing) {
      activeTab = path;
      return;
    }
    
    const name = path.split('/').pop()!.replace(/\.(yml|yaml)$/, '');
    tabs = [...tabs, { path, name, dirty: false }];
    activeTab = path;
  }

  function closeTab(path: string) {
    tabs = tabs.filter(t => t.path !== path);
    if (activeTab === path) {
      activeTab = tabs[tabs.length - 1]?.path || null;
    }
  }

  function switchTab(path: string) {
    activeTab = path;
  }

  // File operations
  function handleSelect(path: string) {
    openTab(path);
  }

  function handleRename(path: string) {
    const name = path.split('/').pop()!;
    const isFile = fs.readFile(path) !== null;
    modalTitle = 'Rename';
    modalInput = isFile ? name.replace(/\.(yml|yaml)$/, '') : name;
    modalType = isFile ? 'rename-file' : 'rename-folder';
    modalData = { oldPath: path };
    showModal = true;
  }

  function handleDelete(path: string) {
    if (confirm(`Delete ${path.split('/').pop()}?`)) {
      fs.deleteItem(path);
      fileTree = buildFileTree();
      if (activeTab === path) {
        closeTab(path);
      }
    }
  }

  function handleNewFile(parentPath: string) {
    modalTitle = 'New File';
    modalInput = '';
    modalType = 'new-file';
    modalData = { parentPath };
    showModal = true;
  }

  function handleNewFolder(parentPath: string) {
    modalTitle = 'New Folder';
    modalInput = '';
    modalType = 'new-folder';
    modalData = { parentPath };
    showModal = true;
  }

  function handleContextMenu(event: { event: MouseEvent; path: string; type: 'file' | 'folder' }) {
    event.event.preventDefault();
    contextMenuPos = { x: event.event.clientX, y: event.event.clientY };
    contextMenuTarget = { path: event.path, type: event.type };
    showContextMenu = true;
  }

  function closeContextMenu() {
    showContextMenu = false;
    contextMenuTarget = null;
  }

  // Modal handling
  function confirmModal() {
    const name = modalInput.trim();
    if (!name) return;
    
    switch (modalType) {
      case 'new-file': {
        const finalName = name.split('.')[0] + '.yml';
        const parentPath = modalData.parentPath || '/';
        const newPath = parentPath === '/' ? '/' + finalName : parentPath + '/' + finalName;
        fs.createFile(newPath, '');
        fileTree = buildFileTree();
        openTab(newPath);
        break;
      }
      case 'new-folder': {
        const parentPath = modalData.parentPath || '/';
        const newPath = parentPath === '/' ? '/' + name : parentPath + '/' + name;
        fs.createFolder(newPath);
        fileTree = buildFileTree();
        break;
      }
      case 'rename-file': {
        const finalName = name.split('.')[0] + '.yml';
        const oldPath = modalData.oldPath;
        const parentPath = oldPath.substring(0, oldPath.lastIndexOf('/')) || '/';
        const newPath = parentPath === '/' ? '/' + finalName : parentPath + '/' + finalName;
        
        const content = fs.readFile(oldPath);
        if (content !== null) {
          fs.deleteItem(oldPath);
          fs.createFile(newPath, content);
          fileTree = buildFileTree();
          
          // Update tabs
          tabs = tabs.map(t => t.path === oldPath ? { ...t, path: newPath, name: finalName.replace(/\.(yml|yaml)$/, '') } : t);
          if (activeTab === oldPath) activeTab = newPath;
        }
        break;
      }
      case 'rename-folder': {
        const oldPath = modalData.oldPath;
        const parentPath = oldPath.substring(0, oldPath.lastIndexOf('/')) || '/';
        const newPath = parentPath === '/' ? '/' + name : parentPath + '/' + name;
        
        // This is simplified - would need recursive move
        fs.deleteItem(oldPath);
        fs.createFolder(newPath);
        fileTree = buildFileTree();
        break;
      }
    }
    showModal = false;
  }

  function cancelModal() {
    showModal = false;
  }

  // Drag and drop
  function handleDragStart(data: { path: string; type: 'file' | 'folder' }) {
    dragSource = data.path;
  }

  function handleDragOver(target: string) {
    if (target !== dragSource) {
      dragOverTarget = target;
    }
  }

  function handleDragLeave() {
    dragOverTarget = null;
  }

  function handleDrop(data: { source: string; target: string }) {
    if (!dragSource) return;
    
    const result = fs.moveItem(data.source, data.target);
    if (result === true) {
      fileTree = buildFileTree();
    } else if (result && result.conflict) {
      // Show overwrite modal
      console.log('Conflict:', result.type);
    }
    dragSource = null;
    dragOverTarget = null;
  }

  function handleDragEnd() {
    dragSource = null;
    dragOverTarget = null;
  }

  // Rendering
  async function render() {
    if (!activeTab || !ymxInstance) return;
    
    status = 'Rendering...';
    try {
      const content = fs.readFile(activeTab);
      if (!content) return;
      
      ymxInstance.clear();
      ymxInstance.parse(content);
      
      const components = ymxInstance.components();
      if (!components.includes('main')) {
        throw new Error('No "main" component found');
      }
      
      const componentFn = ymxInstance.__get__.call(ymxInstance, 'main');
      const result = componentFn(undefined, []);
      
      switch (viewMode) {
        case 'html':
          previewContent = result.asHtml();
          break;
        case 'json':
          previewContent = JSON.stringify(result.json, null, 2);
          break;
        case 'page':
          previewContent = result.asHtml();
          break;
        case 'pdf':
          if (!ironpressLoaded) {
            await loadIronpress();
          }
          const pdfBytes = result.asPdf();
          const blob = new Blob([pdfBytes], { type: 'application/pdf' });
          const url = URL.createObjectURL(blob);
          previewContent = `<embed src="${url}" type="application/pdf" style="width:100%;height:100%;border:none;" />`;
          break;
      }
      status = 'Rendered';
    } catch (err: any) {
      previewContent = `<div class="p-4 text-red-500">Error: ${err.message}</div>`;
      status = 'Error';
    }
  }

  async function loadIronpress() {
    try {
      const ironpress = await import('ironpress');
      ironpressLoaded = true;
    } catch (err) {
      console.error('Ironpress load failed:', err);
    }
  }

  function downloadCurrent() {
    if (!activeTab) return;
    
    const content = fs.readFile(activeTab);
    if (!content) return;
    
    ymxInstance.clear();
    ymxInstance.parse(content);
    
    const components = ymxInstance.components();
    if (!components.includes('main')) return;
    
    const componentFn = ymxInstance.__get__.call(ymxInstance, 'main');
    const result = componentFn(undefined, []);
    
    let downloadContent: string | Uint8Array;
    let filename: string;
    let mimeType: string;
    
    switch (viewMode) {
      case 'html':
        downloadContent = result.asHtml();
        filename = 'output.html';
        mimeType = 'text/html';
        break;
      case 'json':
        downloadContent = JSON.stringify(result.json, null, 2);
        filename = 'output.json';
        mimeType = 'application/json';
        break;
      case 'page':
        downloadContent = result.asHtml();
        filename = 'output.html';
        mimeType = 'text/html';
        break;
      case 'pdf':
        if (!ironpressLoaded) return;
        const pdfBytes = result.asPdf();
        downloadContent = pdfBytes;
        filename = 'output.pdf';
        mimeType = 'application/pdf';
        break;
    }
    
    const blob = new Blob([downloadContent], { type: mimeType });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = filename;
    a.click();
    URL.revokeObjectURL(url);
  }

  function getFileName(path: string) {
    return path.split('/').pop()!.replace(/\.(yml|yaml)$/, '');
  }

  function getFileIcon(type: 'file' | 'folder', collapsed: boolean = false) {
    if (type === 'folder') {
      return collapsed ? Folder : FolderOpen;
    }
    return File;
  }

  // Initialize
  onMount(async () => {
    fs.init();
    fileTree = buildFileTree();
    await initWasm();
    
    // Open first file
    if (fileTree.length > 0) {
      const firstFile = findFirstFile(fileTree);
      if (firstFile) {
        openTab(firstFile.path);
        await render();
      }
    }
    
    // Close context menu on click outside
    document.addEventListener('click', closeContextMenu);
    
    // Handle window resize
    window.addEventListener('resize', () => {
      // Force update
    });
  });

  function findFirstFile(items: FileItem[]): FileItem | null {
    for (const item of items) {
      if (item.type === 'file') return item;
      if (item.children) {
        const found = findFirstFile(item.children);
        if (found) return found;
      }
    }
    return null;
  }

</script>

<div class="h-screen flex flex-col bg-[var(--bg-primary)] text-[var(--fg-primary)]">
  <!-- Header -->
  <header class="h-[var(--header-height)] bg-[var(--bg-secondary)] border-b border-[var(--border)] flex items-center justify-between px-4">
    <div class="flex items-center gap-2">
      <button
        class="p-2 rounded-lg hover:bg-[var(--bg-hover)] transition-colors lg:hidden"
        onclick={() => sidebarOpen = !sidebarOpen}
        aria-label="Toggle sidebar"
      >
        <Menu class="w-5 h-5" />
      </button>
      <h1 class="text-lg font-semibold">YMX Playground</h1>
    </div>
    
    <div class="flex items-center gap-1 bg-[var(--bg-tertiary)] rounded-lg p-1">
      {#each ['html', 'json', 'page', 'pdf'] as mode}
        <button
          class={`
            px-3 py-1.5 rounded-md text-sm font-medium transition-colors
            ${viewMode === mode 
              ? 'bg-[var(--accent)] text-white' 
              : 'text-[var(--fg-muted)] hover:text-[var(--fg-primary)] hover:bg-[var(--bg-hover)]'
            }
          `}
          onclick={() => { viewMode = mode as any; render(); }}
        >
          {mode.toUpperCase()}
        </button>
      {/each}
    </div>
    
    <div class="flex items-center gap-2">
      <span class="text-xs text-[var(--fg-muted)]">{status}</span>
      <button
        class="p-2 rounded-lg hover:bg-[var(--bg-hover)] transition-colors"
        onclick={downloadCurrent}
        aria-label="Download"
      >
        <Download class="w-5 h-5" />
      </button>
    </div>
  </header>

  <!-- Main Content -->
  <div class="flex-1 flex overflow-hidden relative">
    <!-- Sidebar Overlay (Mobile) -->
    {#if !sidebarOpen}
      <div
        class="fixed inset-0 bg-black/50 z-40 lg:hidden"
        onclick={() => sidebarOpen = true}
      />
    {/if}

    <!-- Sidebar -->
    <aside
      class={`
        fixed lg:static inset-y-0 left-0 z-50 w-[var(--sidebar-width)] bg-[var(--bg-secondary)] border-r border-[var(--border)]
        flex flex-col transition-transform duration-200 ease-out lg:transform-none
        ${!sidebarOpen ? '-translate-x-full' : 'translate-x-0'}
      `}
    >
      <div class="h-[var(--editor-header-height)] flex items-center justify-between px-4 border-b border-[var(--border)]">
        <h2 class="text-xs font-semibold uppercase tracking-wider text-[var(--fg-muted)]">Explorer</h2>
        <div class="flex items-center gap-1">
          <button
            class="p-1.5 rounded hover:bg-[var(--bg-hover)] transition-colors"
            onclick={() => handleNewFile('/')}
            aria-label="New file"
          >
            <FilePlus class="w-4 h-4" />
          </button>
          <button
            class="p-1.5 rounded hover:bg-[var(--bg-hover)] transition-colors"
            onclick={() => handleNewFolder('/')}
            aria-label="New folder"
          >
            <FolderPlus class="w-4 h-4" />
          </button>
        </div>
      </div>
      
      <div class="flex-1 overflow-y-auto p-2">
        <FileTree
          items={buildFileTree()}
          activePath={activeTab}
          onSelect={handleSelect}
          onRename={handleRename}
          onDelete={handleDelete}
          onNewFile={handleNewFile}
          onNewFolder={handleNewFolder}
          onContextMenu={handleContextMenu}
          onDragStart={handleDragStart}
          onDragOver={handleDragOver}
          onDragLeave={handleDragLeave}
          onDrop={handleDrop}
          onDragEnd={handleDragEnd}
        />
      </div>
    </aside>

    <!-- Resize Handle -->
    <div
      class="w-px bg-[var(--border)] cursor-col-resize hover:bg-[var(--accent)] lg:hidden"
      style="height: calc(100vh - var(--header-height));"
    />

    <!-- Editor & Preview -->
    <div class="flex-1 flex flex-col lg:flex-row min-w-0 relative">
      <!-- Editor -->
      <div class="flex-1 flex flex-col min-w-0 lg:w-1/2 border-r border-[var(--border)] lg:border-r-0">
        <div class="h-[var(--editor-header-height)] bg-[var(--bg-secondary)] border-b border-[var(--border)] flex items-center justify-between px-3">
          <div class="flex items-center gap-1 overflow-x-auto">
            {#each tabs as tab}
              <button
                class={`
                  px-3 py-1.5 rounded-t-lg text-sm font-medium transition-colors whitespace-nowrap
                  ${activeTab === tab.path
                    ? 'bg-[var(--bg-primary)] text-[var(--fg-primary)] border-b-2 border-[var(--accent)]'
                    : 'bg-[var(--bg-tertiary)] text-[var(--fg-muted)] hover:text-[var(--fg-primary)] hover:bg-[var(--bg-hover)]'
                  }
                `}
                onclick={() => switchTab(tab.path)}
              >
                <span class="flex items-center gap-1">
                  {getFileName(tab.path)}
                  {#if tab.dirty}<span class="text-[var(--warning)]">●</span>{/if}
                </span>
              </button>
            {/each}
            <button
              class="p-2 rounded hover:bg-[var(--bg-hover)] transition-colors"
              onclick={() => handleNewFile('/')}
              aria-label="New file"
            >
              <Plus class="w-4 h-4" />
            </button>
          </div>
          <div class="flex items-center gap-2">
            <button
              class="p-1.5 rounded hover:bg-[var(--bg-hover)] transition-colors"
              onclick={() => { viewMode = 'page'; render(); }}
              aria-label="Preview"
            >
              <ChevronRight class="w-4 h-4" />
            </button>
          </div>
        </div>
        
        <div class="flex-1 relative min-h-0">
          <textarea
            bind:value={editorContent}
            oninput={() => {
              if (activeTab) {
                fs.writeFile(activeTab, editorContent);
                tabs = tabs.map(t => t.path === activeTab ? { ...t, dirty: true } : t);
              }
            }}
            class="w-full h-full bg-[var(--bg-primary)] text-[var(--fg-primary)] border-none resize-none p-4 font-mono text-sm focus:outline-none"
            placeholder="Select a file to edit..."
            spellcheck="false"
          />
        </div>
      </div>

      <!-- Preview -->
      <div class="flex-1 flex flex-col min-w-0 lg:w-1/2">
        <div class="h-[var(--preview-header-height)] bg-[var(--bg-secondary)] border-b border-[var(--border)] flex items-center justify-between px-4">
          <h2 class="text-xs font-semibold uppercase tracking-wider text-[var(--fg-muted)]">Preview</h2>
          <div class="flex items-center gap-2">
            <label class="flex items-center gap-1.5 text-xs text-[var(--fg-muted)] cursor-pointer">
              <input type="checkbox" bind:checked={autoRender} class="accent-[var(--accent)]" />
              Auto-render
            </label>
            <label class="flex items-center gap-1.5 text-xs text-[var(--fg-muted)] cursor-pointer">
              <input type="checkbox" bind:checked={autoLint} class="accent-[var(--accent)]" />
              Auto-lint
            </label>
            <button
              class="px-2 py-1 rounded text-xs bg-[var(--bg-tertiary)] hover:bg-[var(--bg-hover)] transition-colors"
              onclick={render}
            >
              Render
            </button>
          </div>
        </div>
        
        <div class="flex-1 overflow-auto p-4 min-h-0">
          {@html previewContent || '<div class="flex items-center justify-center h-full text-[var(--fg-muted)]"><p>Select a file and render to see preview</p></div>'}
        </div>
      </div>
    </div>
  </div>

  <!-- Context Menu -->
  {#if showContextMenu && contextMenuTarget}
    <div
      class="fixed z-50 bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg shadow-lg min-w-[160px] animate-slide-in"
      style="left: {contextMenuPos.x}px; top: {contextMenuPos.y}px;"
    >
      {#if contextMenuTarget.type === 'folder'}
        <button class="w-full px-3 py-2 text-left text-sm hover:bg-[var(--bg-hover)] flex items-center gap-2" onclick={() => { handleNewFile(contextMenuTarget!.path); closeContextMenu(); }}>
          <FilePlus class="w-4 h-4" /> New File
        </button>
        <button class="w-full px-3 py-2 text-left text-sm hover:bg-[var(--bg-hover)] flex items-center gap-2" onclick={() => { handleNewFolder(contextMenuTarget!.path); closeContextMenu(); }}>
          <FolderPlus class="w-4 h-4" /> New Folder
        </button>
        <hr class="border-[var(--border)] my-1" />
      {/if}
      <button class="w-full px-3 py-2 text-left text-sm hover:bg-[var(--bg-hover)] flex items-center gap-2" onclick={() => { handleRename(contextMenuTarget!.path); closeContextMenu(); }}>
        <Edit class="w-4 h-4" /> Rename
      </button>
      <button class="w-full px-3 py-2 text-left text-sm hover:bg-[var(--bg-hover)] flex items-center gap-2 text-red-500" onclick={() => { handleDelete(contextMenuTarget!.path); closeContextMenu(); }}>
        <Trash2 class="w-4 h-4" /> Delete
      </button>
    </div>
  {/if}

  <!-- Modal -->
  {#if showModal}
    <div class="fixed inset-0 bg-black/50 z-50 flex items-center justify-center animate-slide-in" onclick={cancelModal}>
      <div class="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg p-6 w-full max-w-md animate-slide-in" onclick={(e) => e.stopPropagation()}>
        <h3 class="text-lg font-semibold mb-4">{modalTitle}</h3>
        <div class="mb-4">
          <label class="block text-sm font-medium text-[var(--fg-muted)] mb-1">Name</label>
          <input
            bind:value={modalInput}
            type="text"
            class="w-full px-3 py-2 bg-[var(--bg-primary)] border border-[var(--border)] rounded-lg text-[var(--fg-primary)] focus:outline-none focus:ring-2 focus:ring-[var(--accent)]"
            placeholder={modalType === 'new-file' ? 'filename (without .yml)' : 'folder-name'}
            onkeydown={(e) => { if (e.key === 'Enter') confirmModal(); if (e.key === 'Escape') cancelModal(); }}
          />
        </div>
        <div class="flex justify-end gap-2">
          <button class="px-4 py-2 text-sm bg-[var(--bg-tertiary)] hover:bg-[var(--bg-hover)] rounded-lg transition-colors" onclick={cancelModal}>
            Cancel
          </button>
          <button class="px-4 py-2 text-sm bg-[var(--accent)] hover:bg-[var(--accent-hover)] text-white rounded-lg transition-colors" onclick={confirmModal}>
            {modalType.startsWith('overwrite') ? 'Overwrite' : 'Create'}
          </button>
        </div>
      </div>
    </div>
  {/if}
</div>