// YMX Playground - Main Application
// monaco is loaded globally via AMD loader in index.html

// ============================================================================
// State Management
// ============================================================================

const state = {
    files: new Map(), // path -> { content, dirty, language }
    folders: new Set(), // folder paths
    activeFile: null,
    openTabs: [], // array of file paths
    viewMode: 'html', // html, json, page, pdf
    autoRender: true,
    autoLint: true,
    renderDebounce: null,
    lintDebounce: null,
    ymxInstance: null,
    ironpressLoaded: false,
};

// Drag and drop state
let dragSourcePath = null;


// Track created Monaco models to reuse them
const models = new Map(); // path -> monaco.editor.ITextModel

// Virtual file system (in-memory)
const fs = {
    '/': { type: 'folder', children: {} },
    
    init() {
        console.log('fs.init: starting');
        // Create default example files
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
    console.log('fs.init: completed, state.files keys =', Array.from(state.files.keys()));
    },

    createFolder(path) {
        const parts = path.split('/').filter(Boolean);
        let current = this['/'];
        let currentPath = '';
        
        for (const part of parts) {
            currentPath += '/' + part;
            if (!current.children[part]) {
                current.children[part] = { type: 'folder', children: {} };
                state.folders.add(currentPath);
            }
            current = current.children[part];
        }
    },

    createFile(path, content = '') {
        console.log('createFile: path =', path);
        const parts = path.split('/').filter(Boolean);
        const fileName = parts.pop();
        const folderPath = '/' + parts.join('/');
        
        this.createFolder(folderPath);
        
        let current = this['/'];
        for (const part of parts) {
            current = current.children[part];
        }
        
        current.children[fileName] = { type: 'file', content };
        state.files.set(path, { content, dirty: false, language: this.getLanguage(fileName) });
        console.log('createFile: state.files.set done, path =', path, 'state.files size =', state.files.size);
    },

    deleteItem(path) {
        const parts = path.split('/').filter(Boolean);
        const name = parts.pop();
        const folderPath = '/' + parts.join('/');
        
        let current = this['/'];
        for (const part of parts) {
            current = current.children[part];
        }
        
        if (current.children[name]) {
            delete current.children[name];
            if (current.children[name]?.type === 'folder') {
                this.removeFolderRecursive(path);
            }
            state.files.delete(path);
            state.folders.delete(path);
        }
    },

    
    moveItem(fromPath, toPath) {
        const fromParts = fromPath.split('/').filter(Boolean);
        const toParts = toPath.split('/').filter(Boolean);
        const name = fromParts.pop();
        
        let fromParent = this['/'];
        for (const part of fromParts) {
            fromParent = fromParent.children[part];
        }
        
        let toParent = this['/'];
        for (const part of toParts) {
            toParent = toParent.children[part];
        }
        
        const item = fromParent.children[name];
        if (!item) return false;
        
        if (item.type === 'folder') {
            if (toPath.startsWith(fromPath + '/')) return false;
        }
        
        // Check for file name conflict
        if (item.type === 'file' && toParent.children[name]) {
            // Conflict! Need user confirmation
            return { conflict: true, name, fromPath, toPath, item };
        }
        
        delete fromParent.children[name];
        toParent.children[name] = item;
        
        if (item.type === 'file') {
            const oldFullPath = fromPath === '/' ? '/' + name : fromPath + '/' + name;
            const newFullPath = toPath === '/' ? '/' + name : toPath + '/' + name;
            
            const fileState = state.files.get(oldFullPath);
            if (fileState) {
                state.files.delete(oldFullPath);
                state.files.set(newFullPath, fileState);
                
                const tabIndex = state.openTabs.indexOf(oldFullPath);
                if (tabIndex !== -1) {
                    state.openTabs[tabIndex] = newFullPath;
                }
                if (state.activeFile === oldFullPath) {
                    state.activeFile = newFullPath;
                }
            }
        } else {
            const oldPrefix = fromPath === '/' ? '/' + name : fromPath + '/' + name;
            const newPrefix = toPath === '/' ? '/' + name : toPath + '/' + name;
            
            const filesToMove = [];
            for (const [filePath, fileState] of state.files) {
                if (filePath.startsWith(oldPrefix + '/')) {
                    filesToMove.push({ filePath, fileState });
                }
            }
            for (const { filePath, fileState } of filesToMove) {
                const newPath = filePath.replace(oldPrefix, newPrefix);
                state.files.delete(filePath);
                state.files.set(newPath, fileState);
            }
            
            const foldersToMove = [];
            for (const folderPath of state.folders) {
                if (folderPath.startsWith(oldPrefix + '/')) {
                    foldersToMove.push(folderPath);
                }
            }
            for (const folderPath of foldersToMove) {
                const newPath = folderPath.replace(oldPrefix, newPrefix);
                state.folders.delete(folderPath);
                state.folders.add(newPath);
            }
        }
        
        return true;
    },

    addDragHandlers(row, itemPath, itemType) {
        row.draggable = true;
        
        row.addEventListener("dragstart", (e) => {
            dragSourcePath = itemPath;
            row.classList.add("dragging");
            e.dataTransfer.effectAllowed = "move";
            e.dataTransfer.setData("text/plain", itemPath);
        });
        
        row.addEventListener("dragend", () => {
            row.classList.remove("dragging");
            document.querySelectorAll(".file-tree-item-row.drag-over").forEach(r => r.classList.remove("drag-over"));
            dragSourcePath = null;
        });
        
        row.addEventListener("dragover", (e) => {
            e.preventDefault();
            e.dataTransfer.dropEffect = "move";
            
            if (itemPath !== dragSourcePath) {
                row.classList.add("drag-over");
            }
        });
        
        row.addEventListener("dragleave", () => {
            row.classList.remove("drag-over");
        });
        
        row.addEventListener("drop", (e) => {
            e.preventDefault();
            row.classList.remove("drag-over");
            
            if (dragSourcePath && dragSourcePath !== itemPath) {
                let targetPath = itemPath;
                if (itemType === "file") {
                    // Move to file's parent folder
                    const parts = itemPath.split('/').filter(Boolean);
                    parts.pop();
                    targetPath = '/' + parts.join('/');
                }
                if (!targetPath.startsWith(dragSourcePath + '/')) {
                    const result = fs.moveItem(dragSourcePath, targetPath);
                    if (result && result.conflict) {
                        showOverwriteModal(result.name, result.fromPath, result.toPath, result.item);
                    } else if (result === true) {
                        renderFileTree();
                    }
                }
            }
        });
    },

    
    removeFolderRecursive(path) {
        for (const [filePath] of state.files) {
            if (filePath.startsWith(path + '/')) {
                state.files.delete(filePath);
            }
        }
        for (const folderPath of state.folders) {
            if (folderPath.startsWith(path + '/')) {
                state.folders.delete(folderPath);
            }
        }
    },

    getLanguage(filename) {
        if (filename.endsWith('.yml') || filename.endsWith('.yaml')) return 'yaml';
        if (filename.endsWith('.json')) return 'json';
        if (filename.endsWith('.md')) return 'markdown';
        if (filename.endsWith('.html')) return 'html';
        if (filename.endsWith('.js') || filename.endsWith('.ts')) return 'typescript';
        if (filename.endsWith('.css')) return 'css';
        return 'plaintext';
    },

    readFile(path) {
        const parts = path.split('/').filter(Boolean);
        let current = this['/'];
        for (const part of parts) {
            if (!current.children[part]) return null;
            current = current.children[part];
        }
        return current.type === 'file' ? current.content : null;
    },

    writeFile(path, content) {
        const parts = path.split('/').filter(Boolean);
        const fileName = parts.pop();
        
        let current = this['/'];
        for (const part of parts) {
            current = current.children[part];
        }
        
        if (current.children[fileName] && current.children[fileName].type === 'file') {
            current.children[fileName].content = content;
            const fileState = state.files.get(path);
            if (fileState) {
                fileState.content = content;
                fileState.dirty = true;
            }
        }
    },

    listItems(path = '/') {
        const parts = path.split('/').filter(Boolean);
        let current = this['/'];
        for (const part of parts) {
            if (!current.children[part]) return [];
            current = current.children[part];
        }
        return Object.entries(current.children).map(([name, item]) => ({
            name,
            path: path === '/' ? '/' + name : path + '/' + name,
            ...item
        }));
    }
};

// ============================================================================
// DOM Elements
// ============================================================================

// Elements will be initialized after DOM is ready
let elements = {};

function initElements() {
    elements = {
        fileTree: document.getElementById('file-tree'),
        tabs: document.getElementById('tabs'),
        editor: document.getElementById('editor'),
        previewContent: document.getElementById('preview-content'),
        viewButtons: document.querySelectorAll('.view-btn'),
        autoRender: document.getElementById('auto-render'),
        renderBtn: document.getElementById('render-btn'),
        downloadBtn: document.getElementById('download-btn'),
        status: document.getElementById('status'),
        fileInfo: document.getElementById('file-info'),
        modal: document.getElementById('modal'),
        modalTitle: document.getElementById('modal-title'),
        modalInput: document.getElementById('modal-input'),
        modalForm: document.getElementById('modal-form'),
        modalCancel: document.getElementById('modal-cancel'),
        newFileBtn: document.getElementById('new-file-btn'),
        newFolderBtn: document.getElementById('new-folder-btn'),
        sidebarToggle: document.getElementById('sidebar-toggle'),
        sidebar: document.getElementById('sidebar'),
        sidebarOverlay: document.getElementById('sidebar-overlay'),
        resizeHandle: document.getElementById('resize-handle-vertical'),
        resizeHandleHorizontal: document.getElementById('resize-handle-horizontal'),
        previewContainer: document.getElementById('preview-container'),
        editorContainer: document.querySelector('.editor-container'),
    };
}

// ============================================================================
// Monaco Editor Setup
// ============================================================================

let editor = null;
let currentModel = null;

function initEditor() {
    // Configure Monaco
    monaco.editor.defineTheme('ymx-dark', {
        base: 'vs-dark',
        inherit: true,
        rules: [],
        colors: {
            'editor.background': '#1e1e1e',
            'editor.foreground': '#d4d4d4',
            'editor.lineHighlightBackground': '#2d2d2d',
            'editorCursor.foreground': '#ffffff',
            'editor.selectionBackground': '#264f78',
        }
    });

    editor = monaco.editor.create(elements.editor, {
        value: '',
        language: 'yaml',
        theme: 'ymx-dark',
        automaticLayout: true,
        minimap: { enabled: false },
        fontSize: 13,
        lineNumbers: 'on',
        scrollBeyondLastLine: false,
        wordWrap: 'on',
        tabSize: 2,
        insertSpaces: true,
        bracketPairColorization: { enabled: true },
        guides: { bracketPairs: true },
        glyphMargin: true,
        lineDecorationsWidth: 10,
    });

    editor.onDidChangeModelContent(() => {
        if (state.activeFile) {
            const fileState = state.files.get(state.activeFile);
            if (fileState) {
                fileState.content = editor.getValue();
                fileState.dirty = true;
                updateTabDirty(state.activeFile, true);
                scheduleRender();
                scheduleLint();
            }
        }
    });
}

// ============================================================================
// File Tree Rendering
// ============================================================================

function renderFileTree() {
    elements.fileTree.innerHTML = '';
    renderFolder('/', '/', 0);
}

function renderFolder(path, displayPath, depth) {
    const items = fs.listItems(path).sort((a, b) => {
        // Folders first, then files
        if (a.type !== b.type) return a.type === 'folder' ? -1 : 1;
        return a.name.localeCompare(b.name);
    });

    for (const item of items) {
        const el = document.createElement('div');
        el.className = `file-tree-item ${item.type === 'folder' ? 'file-tree-folder' : ''}`;
        el.dataset.path = item.path;
        
        const row = document.createElement('div');
        row.className = 'file-tree-item-row';
        
        if (item.type === 'folder') {
            const isCollapsed = el.classList.contains('collapsed');
            row.innerHTML = `
                <span class="icon">📁</span>
                <span class="name">${item.name}</span>
            `;
            
            const childrenContainer = document.createElement('div');
            childrenContainer.className = 'file-tree-children';
            if (isCollapsed) childrenContainer.style.display = 'none';
            
            // Click on folder name or icon toggles expand/collapse
            row.addEventListener('click', (e) => {
                if (e.target === row || e.target === row.querySelector('.name') || e.target === row.querySelector('.icon')) {
                    const isCollapsed = el.classList.toggle('collapsed');
                    childrenContainer.style.display = isCollapsed ? 'none' : 'block';
                }
            });
            
            el.appendChild(row);
            el.appendChild(childrenContainer);
            fs.addDragHandlers(row, item.path, "folder");
            renderFolderIntoContainer(item.path, item.path, depth + 1, childrenContainer);
        } else {
            const isActive = state.activeFile === item.path;
            const isDirty = state.files.get(item.path)?.dirty;
            row.innerHTML = `
                <span class="icon">📄</span>
                <span class="name ${isDirty ? 'dirty' : ''}">${item.name}</span>
            `;
            if (isActive) el.classList.add('active');
            
            row.addEventListener('click', () => openFile(item.path));
            row.addEventListener('dblclick', () => renameItem(item.path));
            fs.addDragHandlers(row, item.path, "file");
            
            el.appendChild(row);
        }
        
        
        if (depth === 0) {
            elements.fileTree.appendChild(el);
        } else {
            // This is handled by the parent folder's childrenContainer
        }
        
        // Context menu for root level items
        el.addEventListener('contextmenu', (e) => {
            e.preventDefault();
            e.stopPropagation();
            showContextMenu(e.clientX, e.clientY, item.path, item.type);
        });
    }
}

// Helper to render into a specific container
function renderFolderIntoContainer(path, displayPath, depth, container) {
    const items = fs.listItems(path).sort((a, b) => {
        if (a.type !== b.type) return a.type === 'folder' ? -1 : 1;
        return a.name.localeCompare(b.name);
    });

    for (const item of items) {
        const el = document.createElement('div');
        el.className = `file-tree-item ${item.type === 'folder' ? 'file-tree-folder' : ''}`;
        el.dataset.path = item.path;
        
        const row = document.createElement('div');
        row.className = 'file-tree-item-row';
        
        if (item.type === 'folder') {
            const isCollapsed = el.classList.contains('collapsed');
            row.innerHTML = `
                <span class="icon">📁</span>
                <span class="name">${item.name}</span>
            `;
            
            const childrenContainer = document.createElement('div');
            childrenContainer.className = 'file-tree-children';
            if (isCollapsed) childrenContainer.style.display = 'none';
            
            // Click on folder name or icon toggles expand/collapse
            row.addEventListener('click', (e) => {
                if (e.target === row || e.target === row.querySelector('.name') || e.target === row.querySelector('.icon')) {
                    const isCollapsed = el.classList.toggle('collapsed');
                    childrenContainer.style.display = isCollapsed ? 'none' : 'block';
                }
            });
            
            container.appendChild(el);
            el.appendChild(row);
            el.appendChild(childrenContainer);
            fs.addDragHandlers(row, item.path, "folder");
            renderFolderIntoContainer(item.path, item.path, depth + 1, childrenContainer);
        } else {
            const isActive = state.activeFile === item.path;
            const isDirty = state.files.get(item.path)?.dirty;
            row.innerHTML = `
                <span class="icon">📄</span>
                <span class="name ${isDirty ? 'dirty' : ''}">${item.name}</span>
            `;
            if (isActive) el.classList.add('active');
            
            row.addEventListener('click', () => openFile(item.path));
            row.addEventListener('dblclick', () => renameItem(item.path));
            
            fs.addDragHandlers(row, item.path, "file");
            container.appendChild(el);
            el.appendChild(row);
        }
        
        el.addEventListener('contextmenu', (e) => {
            e.preventDefault();
            e.stopPropagation();
            showContextMenu(e.clientX, e.clientY, item.path, item.type);
        });
    }
}

// ============================================================================
// Tab Management
// ============================================================================

function openFile(path) {
    const fileState = state.files.get(path);
    if (!fileState) return;

    // Check if already open
    const existingTabIndex = state.openTabs.indexOf(path);
    if (existingTabIndex !== -1) {
        switchTab(path);
        return;
    }

    // Add to open tabs
    state.openTabs.push(path);
    state.activeFile = path;
    fileState.dirty = false;

    // Reuse existing model or create new one
    let model = models.get(path);
    if (!model) {
        const language = fileState.language;
        model = monaco.editor.createModel(fileState.content, language, monaco.Uri.file(path));
        models.set(path, model);
    }
    editor.setModel(model);

    updateTabs();
    updateFileTreeSelection();
    updateFileInfo();
    scheduleRender();
}

function switchTab(path) {
    if (state.activeFile === path) return;
    
    // Save current editor content
    if (state.activeFile && currentModel) {
        const content = editor.getValue();
        const fileState = state.files.get(state.activeFile);
        if (fileState) {
            fileState.content = content;
        }
    }

    state.activeFile = path;
    const fileState = state.files.get(path);
    
    if (fileState) {
        // Reuse existing model or create new one
        let model = models.get(path);
        if (!model) {
            model = monaco.editor.createModel(fileState.content, fileState.language, monaco.Uri.file(path));
            models.set(path, model);
        }
        editor.setModel(model);
    }

    updateTabs();
    updateFileTreeSelection();
    updateFileInfo();
    scheduleRender();
}

function closeTab(path) {
    const index = state.openTabs.indexOf(path);
    if (index === -1) return;

    state.openTabs.splice(index, 1);
    
    // Dispose model if no other tabs reference it
    const model = models.get(path);
    if (model && !state.openTabs.includes(path)) {
        model.dispose();
        models.delete(path);
    }
    
    if (state.activeFile === path) {
        if (state.openTabs.length > 0) {
            const newActive = state.openTabs[Math.min(index, state.openTabs.length - 1)];
            switchTab(newActive);
        } else {
            state.activeFile = null;
            editor.setModel(null);
            currentModel = null;
        }
    }

    updateTabs();
    updateFileTreeSelection();
    updateFileInfo();
    scheduleRender();
}

function updateTabs() {
    elements.tabs.innerHTML = '';
    
    for (const path of state.openTabs) {
        const fileState = state.files.get(path);
        if (!fileState) continue;
        
        const isActive = state.activeFile === path;
        const tab = document.createElement('div');
        tab.className = `tab ${isActive ? 'active' : ''}`;
        tab.dataset.path = path;
        tab.innerHTML = `
            <span>${getFileName(path)}</span>
            <span class="close">×</span>
        `;
        
        tab.addEventListener('click', (e) => {
            if (e.target.classList.contains('close')) {
                closeTab(path);
            } else {
                switchTab(path);
            }
        });
        
        elements.tabs.appendChild(tab);
    }
}

function updateTabDirty(path, dirty) {
    const tab = elements.tabs.querySelector(`[data-path="${path}"]`);
    if (tab) {
        const nameEl = tab.querySelector('span');
        if (dirty) {
            nameEl.textContent = getFileName(path) + ' ●';
        } else {
            nameEl.textContent = getFileName(path);
        }
    }
    
    // Also update file tree
    const treeItem = elements.fileTree.querySelector(`[data-path="${path}"] .name`);
    if (treeItem) {
        treeItem.classList.toggle('dirty', dirty);
    }
}

function updateFileTreeSelection() {
    document.querySelectorAll('.file-tree-item.active').forEach(el => el.classList.remove('active'));
    if (state.activeFile) {
        const activeEl = elements.fileTree.querySelector(`[data-path="${state.activeFile}"]`);
        if (activeEl) activeEl.classList.add('active');
    }
}

function updateFileInfo() {
    if (state.activeFile) {
        const fileState = state.files.get(state.activeFile);
        elements.fileInfo.textContent = `${getFileName(state.activeFile)} • ${fileState?.language || 'plaintext'} • ${editor.getModel()?.getLineCount() || 0} lines`;
    } else {
        elements.fileInfo.textContent = 'No file selected';
    }
}

function getFileName(path) {
    const name = path.split("/").pop() || path;
    return name.replace(/\.(yml|yaml)$/, "");
}

// ============================================================================
// Context Menu
// ============================================================================

let contextMenu = null;

function showContextMenu(x, y, path, type) {
    hideContextMenu();
    
    contextMenu = document.createElement('div');
    contextMenu.className = 'context-menu';
    contextMenu.style.cssText = `
        position: fixed;
        left: ${x}px;
        top: ${y}px;
        background: var(--bg-secondary);
        border: 1px solid var(--border);
        border-radius: 6px;
        padding: 6px 0;
        min-width: 180px;
        z-index: 1000;
        box-shadow: 0 4px 12px rgba(0,0,0,0.4);
    `;
    
    const items = [];
    
    if (type === 'folder') {
        items.push({ label: 'New File', action: () => showNewFileModal(path) });
        items.push({ label: 'New Folder', action: () => showNewFolderModal(path) });
        items.push({ type: 'separator' });
        items.push({ label: 'Rename', action: () => renameItem(path) });
        items.push({ label: 'Delete', action: () => deleteItem(path), danger: true });
    } else {
        items.push({ label: 'Rename', action: () => renameItem(path) });
        items.push({ label: 'Delete', action: () => deleteItem(path), danger: true });
    }
    
    for (const item of items) {
        if (item.type === 'separator') {
            const sep = document.createElement('div');
            sep.style.cssText = 'height: 1px; background: var(--border); margin: 6px 0;';
            contextMenu.appendChild(sep);
        } else {
            const btn = document.createElement('button');
            btn.textContent = item.label;
            btn.style.cssText = `
                width: 100%;
                padding: 8px 12px;
                border: none;
                background: transparent;
                color: ${item.danger ? 'var(--error)' : 'var(--fg-primary)'};
                font-size: 0.85rem;
                text-align: left;
                cursor: pointer;
            `;
            btn.addEventListener('mouseenter', () => btn.style.background = 'var(--bg-hover)');
            btn.addEventListener('mouseleave', () => btn.style.background = 'transparent');
            btn.addEventListener('click', () => {
                item.action();
                hideContextMenu();
            });
            contextMenu.appendChild(btn);
        }
    }
    
    document.body.appendChild(contextMenu);
    
    // Close on click outside
    setTimeout(() => {
        document.addEventListener('click', hideContextMenu, { once: true });
    }, 0);
}

function hideContextMenu() {
    if (contextMenu) {
        contextMenu.remove();
        contextMenu = null;
    }
}

// ============================================================================
// File Operations
// ============================================================================

function showNewFileModal(parentPath = '/') {
    elements.modalTitle.textContent = 'New File';
    elements.modalInput.value = '';
    elements.modalInput.placeholder = 'filename (without .yml)';
    elements.modal.dataset.parentPath = parentPath;
    elements.modalInput.dataset.type = 'file';
    showModal();
}

function showNewFolderModal(parentPath = '/') {
    elements.modalTitle.textContent = 'New Folder';
    elements.modalInput.value = '';
    elements.modalInput.placeholder = 'folder-name';
    elements.modal.dataset.parentPath = parentPath;
    elements.modalInput.dataset.type = 'folder';
    showModal();
}

function showModal() {
    elements.modal.classList.add('open');
    elements.modalInput.focus();
}

function hideModal() {
    elements.modal.classList.remove('open');
}

function renameItem(path) {
    const name = path.split('/').pop();
    const isFile = state.files.has(path);
    const baseName = isFile ? name.replace(/\.(yml|yaml)$/, '') : name;
    
    elements.modalTitle.textContent = 'Rename';
    elements.modalInput.value = baseName;
    elements.modalInput.placeholder = isFile ? 'filename (without .yml)' : 'folder-name';
    elements.modal.dataset.parentPath = path;
    elements.modalInput.dataset.type = isFile ? 'rename-file' : 'rename-folder';
    elements.modalInput.dataset.oldPath = path;
    
    showModal();
}

function handleRename(oldPath, newName, type) {
    if (!newName || newName === oldPath.split('/').pop()) return;
    
    // Force .yml extension (take first word before first dot)
    if (type === 'rename-file') {
        newName = newName.split('.')[0] + '.yml';
    }
    
    const parentPath = oldPath.substring(0, oldPath.lastIndexOf('/')) || '/';
    const newPath = parentPath === '/' ? '/' + newName : parentPath + '/' + newName;
    
    // For files, we need to update state.files
    if (state.files.has(oldPath)) {
        const fileState = state.files.get(oldPath);
        state.files.delete(oldPath);
        state.files.set(newPath, fileState);
        
        // Update open tabs
        const tabIndex = state.openTabs.indexOf(oldPath);
        if (tabIndex !== -1) {
            state.openTabs[tabIndex] = newPath;
        }
        if (state.activeFile === oldPath) {
            state.activeFile = newPath;
        }
    }
    
    // Update virtual FS
    const content = fs.readFile(oldPath);
    if (content !== null) {
        fs.deleteItem(oldPath);
        fs.createFile(newPath, content);
    }
    
    renderFileTree();
    updateTabs();
    updateFileTreeSelection();
}

function showOverwriteModal(fileName, fromPath, toPath, item) {
    elements.modalTitle.textContent = 'File Exists';
    elements.modalInput.value = fileName;
    elements.modalInput.placeholder = 'File name';
    elements.modal.dataset.fromPath = fromPath;
    elements.modal.dataset.toPath = toPath;
    elements.modalInput.dataset.type = 'overwrite-file';
    elements.modalInput.dataset.item = JSON.stringify(item);
    // Change modal buttons
    const submitBtn = elements.modalForm.querySelector('button[type="submit"]');
    submitBtn.textContent = 'Overwrite';
    showModal();
}

function executeMove(fromPath, toPath, item) {
    const fromParts = fromPath.split('/').filter(Boolean);
    const toParts = toPath.split('/').filter(Boolean);
    const name = fromParts.pop();
    
    let fromParent = fs['/'];
    for (const part of fromParts) {
        fromParent = fromParent.children[part];
    }
    
    let toParent = fs['/'];
    for (const part of toParts) {
        toParent = toParent.children[part];
    }
    
    // Remove existing file
    delete toParent.children[name];
    
    // Move the item
    delete fromParent.children[name];
    toParent.children[name] = item;
    
    if (item.type === 'file') {
        const oldFullPath = fromPath === '/' ? '/' + name : fromPath + '/' + name;
        const newFullPath = toPath === '/' ? '/' + name : toPath + '/' + name;
        
        const fileState = state.files.get(oldFullPath);
        if (fileState) {
            state.files.delete(oldFullPath);
            state.files.set(newFullPath, fileState);
            
            const tabIndex = state.openTabs.indexOf(oldFullPath);
            if (tabIndex !== -1) {
                state.openTabs[tabIndex] = newFullPath;
            }
            if (state.activeFile === oldFullPath) {
                state.activeFile = newFullPath;
            }
        }
    } else {
        const oldPrefix = fromPath === '/' ? '/' + name : fromPath + '/' + name;
        const newPrefix = toPath === '/' ? '/' + name : toPath + '/' + name;
        
        const filesToMove = [];
        for (const [filePath, fileState] of state.files) {
            if (filePath.startsWith(oldPrefix + '/')) {
                filesToMove.push({ filePath, fileState });
            }
        }
        for (const { filePath, fileState } of filesToMove) {
            const newPath = filePath.replace(oldPrefix, newPrefix);
            state.files.delete(filePath);
            state.files.set(newPath, fileState);
        }
        
        const foldersToMove = [];
        for (const folderPath of state.folders) {
            if (folderPath.startsWith(oldPrefix + '/')) {
                foldersToMove.push(folderPath);
            }
        }
        for (const folderPath of foldersToMove) {
            const newPath = folderPath.replace(oldPrefix, newPrefix);
            state.folders.delete(folderPath);
            state.folders.add(newPath);
        }
    }
    
    return true;
}

function deleteItem(path) {
    if (!confirm(`Delete ${getFileName(path)}?`)) return;
    
    // Close tab if open
    if (state.openTabs.includes(path)) {
        closeTab(path);
    }
    
    fs.deleteItem(path);
    renderFileTree();
    scheduleRender();
}

function setupEventListeners() {
    // Sidebar toggle for mobile
    elements.sidebarToggle.addEventListener('click', () => {
        elements.sidebar.classList.toggle('open');
        elements.sidebarOverlay.classList.toggle('open');
    });

    elements.sidebarOverlay.addEventListener('click', () => {
        elements.sidebar.classList.remove('open');
        elements.sidebarOverlay.classList.remove('open');
    });

    // Close sidebar on file click (mobile)
    document.addEventListener('click', (e) => {
        if (window.innerWidth <= 900 && elements.sidebar.classList.contains('open')) {
            const sidebar = elements.sidebar;
            const toggle = elements.sidebarToggle;
            if (!sidebar.contains(e.target) && !toggle.contains(e.target)) {
                elements.sidebar.classList.remove('open');
                elements.sidebarOverlay.classList.remove('open');
            }
        }
    });

    // Modal event listeners
    elements.modalForm.addEventListener('submit', (e) => {
        e.preventDefault();
            e.stopPropagation();
        let name = elements.modalInput.value.trim();
        const type = elements.modalInput.dataset.type;
        const parentPath = elements.modal.dataset.parentPath || '/';
        
        if (!name) return;
        
        // Handle rename operations
        if (type === 'rename-file' || type === 'rename-folder') {
            const oldPath = elements.modalInput.dataset.oldPath;
            handleRename(oldPath, name, type);
            hideModal();
            return;
        }
        
        if (type === 'overwrite-file') {
            const fromPath = elements.modal.dataset.fromPath;
            const toPath = elements.modal.dataset.toPath;
            const item = JSON.parse(elements.modalInput.dataset.item);
            executeMove(fromPath, toPath, item);
            renderFileTree();
            hideModal();
            // Reset submit button text
            const submitBtn = elements.modalForm.querySelector('button[type="submit"]');
            submitBtn.textContent = 'Create';
            return;
        }
        
        // Force .yml extension (take first word before first dot)
        if (type === 'file') {
            name = name.split('.')[0] + '.yml';
        }
        
        const newPath = parentPath === '/' ? '/' + name : parentPath + '/' + name;
        
        if (type === 'folder') {
            fs.createFolder(newPath);
        } else {
            fs.createFile(newPath, '');
            // Auto-open the new file
            setTimeout(() => openFile(newPath), 0);
        }
        
        renderFileTree();
        hideModal();
    });

    elements.modalCancel.addEventListener('click', hideModal);
    elements.modal.addEventListener('click', (e) => {
        if (e.target === elements.modal) hideModal();
    });

    // New file/folder buttons
    elements.newFileBtn.addEventListener('click', () => showNewFileModal('/'));
    elements.newFolderBtn.addEventListener('click', () => showNewFolderModal('/'));

    // Resize handle for mobile preview
    initResizeHandle();

    // View mode buttons
    elements.viewButtons.forEach(btn => {
        btn.addEventListener('click', () => {
            elements.viewButtons.forEach(b => {
                b.classList.remove('active');
                b.setAttribute('aria-selected', 'false');
            });
            btn.classList.add('active');
            btn.setAttribute('aria-selected', 'true');
            state.viewMode = btn.dataset.view;
            scheduleRender();
        });
    });

    // Auto-render checkbox
    elements.autoRender.addEventListener('change', (e) => {
        state.autoRender = e.target.checked;
        if (state.autoRender) scheduleRender();
    });

    // Auto-lint checkbox and lint/render buttons
    elements.autoLint = document.getElementById('auto-lint');
    elements.lintBtn = document.getElementById('lint-btn');

    elements.autoLint.addEventListener('change', (e) => {
        state.autoLint = e.target.checked;
        if (state.autoLint) scheduleLint();
    });

    elements.lintBtn.addEventListener('click', lint);
    elements.renderBtn.addEventListener('click', render);

    // Download button
    elements.downloadBtn.addEventListener('click', async () => {
        if (!state.activeFile || !state.ymxInstance) return;
        
        const fileState = state.files.get(state.activeFile);
        if (!fileState) return;
        
        try {
            // Build a Map of all .yml files in the virtual filesystem
            const fileMap = new Map();
            for (const [path, f] of state.files) {
                if (path.endsWith('.yml') || path.endsWith('.yaml')) {
                    fileMap.set(path, f.content);
                }
            }
            
            // Clear and parse all files (handles _use imports)
            state.ymxInstance.clear();
            state.ymxInstance.parse_map(fileMap);
            
            const components = state.ymxInstance.components();
            const mainComponent = 'main';
            
            if (!components.includes(mainComponent)) {
                throw new Error('No "main" component found. Please define a "main" component in your YAML to render.');
            }
            
            const componentFn = state.ymxInstance.__get__.call(state.ymxInstance, mainComponent);
            const result = componentFn(undefined, []);
            
            let content, filename, mimeType;
            
            switch (state.viewMode) {
                case 'html':
                    content = result.asHtml();
                    filename = 'output.html';
                    mimeType = 'text/html';
                    break;
                case 'json':
                    content = JSON.stringify(result.json, null, 2);
                    filename = 'output.json';
                    mimeType = 'application/json';
                    break;
                case 'page':
                    content = result.asHtml();
                    filename = 'output.html';
                    mimeType = 'text/html';
                    break;
                case 'pdf':
                    if (!state.ironpressLoaded) {
                        await loadIronpress();
                    }
                    const html = result.asHtml();
                    const pdfBytes = await window.ironpress.pdf(html, {
                        format: 'A4',
                        margin: { top: '1cm', right: '1cm', bottom: '1cm', left: '1cm' }
                    });
                    content = pdfBytes;
                    filename = 'output.pdf';
                    mimeType = 'application/pdf';
                    break;
            }
            
            const blob = new Blob([content], { type: mimeType });
            const url = URL.createObjectURL(blob);
            const a = document.createElement('a');
            a.href = url;
            a.download = filename;
            a.click();
            URL.revokeObjectURL(url);
        } catch (err) {
            console.error('Download failed:', err);
            showToast('Download failed: ' + err.message);
        }
    });
}

function initResizeHandle() {
    const editorContainer = elements.editorContainer;
    const previewContainer = elements.previewContainer;
    
    if (!editorContainer || !previewContainer) return;
    
    // Initialize both handles
    initVerticalResizeHandle();
    initHorizontalResizeHandle();
}

function initVerticalResizeHandle() {
    const handle = elements.resizeHandle;
    if (!handle) return;
    
    const isMobile = window.innerWidth <= 900;
    if (!isMobile) return; // Vertical handle only on mobile
    
    // Mouse events
    handle.addEventListener('mousedown', (e) => startVerticalDrag(e, handle));
    handle.addEventListener('touchstart', (e) => startVerticalDrag(e, handle), { passive: false });
    
    // Keyboard support
    handle.addEventListener('keydown', (e) => {
        if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
            e.preventDefault();
            e.stopPropagation();
            const delta = e.key === 'ArrowUp' ? -20 : 20;
            adjustVerticalSplit(delta);
        }
    });
}

function initHorizontalResizeHandle() {
    const handle = elements.resizeHandleHorizontal;
    if (!handle) return;
    
    const isDesktop = window.innerWidth > 900;
    if (!isDesktop) return; // Horizontal handle only on desktop
    
    // Mouse events
    handle.addEventListener('mousedown', (e) => startHorizontalDrag(e, handle));
    handle.addEventListener('touchstart', (e) => startHorizontalDrag(e, handle), { passive: false });
    
    // Keyboard support
    handle.addEventListener('keydown', (e) => {
        if (e.key === 'ArrowLeft' || e.key === 'ArrowRight') {
            e.preventDefault();
            e.stopPropagation();
            const delta = e.key === 'ArrowLeft' ? -20 : 20;
            adjustHorizontalSplit(delta);
        }
    });
}

// State for vertical drag
const verticalResizeState = {
    isDragging: false,
    startY: 0,
    editorStartHeight: 0,
    previewStartHeight: 0,
};

// State for horizontal drag
const horizontalResizeState = {
    isDragging: false,
    startX: 0,
    editorStartWidth: 0,
    previewStartWidth: 0,
};

function startVerticalDrag(e, handle) {
    if (e.type === 'mousedown' && e.button !== 0) return;
    
    e.preventDefault();
            e.stopPropagation();
    e.stopPropagation();
    
    verticalResizeState.isDragging = true;
    verticalResizeState.startY = e.type === 'mousedown' ? e.clientY : e.touches[0].clientY;
    verticalResizeState.editorStartHeight = elements.editorContainer.offsetHeight;
    verticalResizeState.previewStartHeight = elements.previewContainer.offsetHeight;
    
    handle.classList.add('dragging');
    document.body.style.cursor = 'row-resize';
    document.body.style.userSelect = 'none';
    
    document.addEventListener('mousemove', onVerticalDrag);
    document.addEventListener('mouseup', endVerticalDrag);
    document.addEventListener('touchmove', onVerticalDrag, { passive: false });
    document.addEventListener('touchend', endVerticalDrag);
}

function onVerticalDrag(e) {
    if (!verticalResizeState.isDragging) return;
    
    e.preventDefault();
            e.stopPropagation();
    
    const currentY = e.type === 'mousemove' ? e.clientY : e.touches[0].clientY;
    const deltaY = currentY - verticalResizeState.startY;
    
    // Handle is on bottom edge of editor: dragging DOWN makes editor TALLER
    const newEditorHeight = verticalResizeState.editorStartHeight + deltaY;
    const newPreviewHeight = verticalResizeState.previewStartHeight - deltaY;
    
    const minEditor = 150;
    const isMobile = window.innerWidth <= 600;
    const isTablet = window.innerWidth <= 900;
    const minPreview = isMobile ? 180 : (isTablet ? 200 : 120);
    const maxPreview = window.innerHeight * 0.8;
    
    if (newEditorHeight >= minEditor && newPreviewHeight >= minPreview && newPreviewHeight <= maxPreview) {
        elements.editorContainer.style.flex = 'none';
        elements.editorContainer.style.height = newEditorHeight + 'px';
        elements.previewContainer.style.flex = 'none';
        elements.previewContainer.style.height = newPreviewHeight + 'px';
    }
}

function endVerticalDrag() {
    if (!verticalResizeState.isDragging) return;
    
    verticalResizeState.isDragging = false;
    const handle = elements.resizeHandle;
    if (handle) handle.classList.remove('dragging');
    document.body.style.cursor = '';
    document.body.style.userSelect = '';
    
    document.removeEventListener('mousemove', onVerticalDrag);
    document.removeEventListener('mouseup', endVerticalDrag);
    document.removeEventListener('touchmove', onVerticalDrag);
    document.removeEventListener('touchend', endVerticalDrag);
}

function adjustVerticalSplit(delta) {
    const editorHeight = elements.editorContainer.offsetHeight;
    const previewHeight = elements.previewContainer.offsetHeight;
    
    // ArrowUp (negative delta) = editor shorter, ArrowDown (positive delta) = editor taller
    const newEditorHeight = editorHeight + delta;
    const newPreviewHeight = previewHeight - delta;
    
    const isMobile = window.innerWidth <= 600;
    const isTablet = window.innerWidth <= 900;
    const minEditor = 150;
    const minPreview = isMobile ? 180 : (window.innerWidth <= 900 ? 200 : 120);
    const maxPreview = window.innerHeight * 0.8;
    
    if (newEditorHeight >= minEditor && newPreviewHeight >= minPreview && newPreviewHeight <= maxPreview) {
        elements.editorContainer.style.flex = 'none';
        elements.editorContainer.style.height = newEditorHeight + 'px';
        elements.previewContainer.style.flex = 'none';
        elements.previewContainer.style.height = newPreviewHeight + 'px';
    }
}

function startHorizontalDrag(e, handle) {
    if (e.type === 'mousedown' && e.button !== 0) return;
    
    e.preventDefault();
            e.stopPropagation();
    e.stopPropagation();
    
    horizontalResizeState.isDragging = true;
    horizontalResizeState.startX = e.type === 'mousedown' ? e.clientX : e.touches[0].clientX;
    horizontalResizeState.editorStartWidth = elements.editorContainer.offsetWidth;
    horizontalResizeState.previewStartWidth = elements.previewContainer.offsetWidth;
    
    handle.classList.add('dragging');
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';
    
    document.addEventListener('mousemove', onHorizontalDrag);
    document.addEventListener('mouseup', endHorizontalDrag);
    document.addEventListener('touchmove', onHorizontalDrag, { passive: false });
    document.addEventListener('touchend', endHorizontalDrag);
}

function onHorizontalDrag(e) {
    if (!horizontalResizeState.isDragging) return;
    
    e.preventDefault();
            e.stopPropagation();
    
    const currentX = e.type === 'mousemove' ? e.clientX : e.touches[0].clientX;
    const deltaX = currentX - horizontalResizeState.startX;
    
    // Handle is on right edge of editor: dragging RIGHT makes editor WIDER
    const newEditorWidth = horizontalResizeState.editorStartWidth + deltaX;
    const newPreviewWidth = horizontalResizeState.previewStartWidth - deltaX;
    
    const minEditor = 300;
    const minPreview = 200;
    const maxPreview = window.innerWidth * 0.8;
    
    if (newEditorWidth >= minEditor && newPreviewWidth >= minPreview && newPreviewWidth <= maxPreview) {
        elements.editorContainer.style.flex = 'none';
        elements.editorContainer.style.width = newEditorWidth + 'px';
        elements.previewContainer.style.flex = 'none';
        elements.previewContainer.style.width = newPreviewWidth + 'px';
    }
}

function endHorizontalDrag() {
    if (!horizontalResizeState.isDragging) return;
    
    horizontalResizeState.isDragging = false;
    const handle = elements.resizeHandleHorizontal;
    if (handle) handle.classList.remove('dragging');
    document.body.style.cursor = '';
    document.body.style.userSelect = '';
    
    document.removeEventListener('mousemove', onHorizontalDrag);
    document.removeEventListener('mouseup', endHorizontalDrag);
    document.removeEventListener('touchmove', onHorizontalDrag);
    document.removeEventListener('touchend', endHorizontalDrag);
}

function adjustHorizontalSplit(delta) {
    const editorWidth = elements.editorContainer.offsetWidth;
    const previewWidth = elements.previewContainer.offsetWidth;
    
    // ArrowLeft (negative delta) = editor narrower, ArrowRight (positive delta) = editor wider
    const newEditorWidth = editorWidth + delta;
    const newPreviewWidth = previewWidth - delta;
    
    const minEditor = 300;
    const minPreview = 200;
    const maxPreview = window.innerWidth * 0.8;
    
    if (newEditorWidth >= minEditor && newPreviewWidth >= minPreview && newPreviewWidth <= maxPreview) {
        elements.editorContainer.style.flex = 'none';
        elements.editorContainer.style.width = newEditorWidth + 'px';
        elements.previewContainer.style.flex = 'none';
        elements.previewContainer.style.width = newPreviewWidth + 'px';
    }
}

// Initialize on load
document.addEventListener('DOMContentLoaded', initResizeHandle);

// Re-init on window resize
window.addEventListener('resize', () => {
    if (window.innerWidth > 900) {
        // Reset styles on desktop
        if (elements.editorContainer) {
            elements.editorContainer.style.flex = '';
            elements.editorContainer.style.height = '';
            elements.editorContainer.style.width = '';
        }
        if (elements.previewContainer) {
            elements.previewContainer.style.flex = '';
            elements.previewContainer.style.height = '';
            elements.previewContainer.style.width = '';
        }
    } else {
        // Reset styles on mobile
        if (elements.editorContainer) {
            elements.editorContainer.style.flex = '';
            elements.editorContainer.style.height = '';
        }
        if (elements.previewContainer) {
            elements.previewContainer.style.flex = '';
            elements.previewContainer.style.height = '';
        }
    }
    
    // Reinitialize resize handles on breakpoint change
    initResizeHandle();
});

// ============================================================================
// WASM Integration
// ============================================================================

async function initWasm() {
    try {
        updateStatus('Loading WASM...');
        
        // Import the WASM module
        const wasmModule = await import('./pkg/ymx_web.js');
        await wasmModule.default();
        
        state.ymxInstance = new wasmModule.Ymx();
        
        // Initialize ironpress if available
        if (typeof ironpress !== 'undefined' && typeof ironpress.default === 'function') {
            try {
                await ironpress.default();
                state.ironpressLoaded = true;
            } catch (e) {
                console.warn('Failed to initialize ironpress:', e);
            }
        }
        
        updateStatus('Ready');
        scheduleRender();
    } catch (error) {
        console.error('Failed to load WASM:', error);
        updateStatus('WASM load failed');
        showToast('Failed to load YMX WASM module');
    }
}

function updateStatus(text) {
    elements.status.textContent = text;
}

function showToast(message) {
    const toast = document.createElement('div');
    toast.className = 'toast';
    toast.textContent = message;
    document.body.appendChild(toast);
    setTimeout(() => toast.remove(), 3000);
}

// ============================================================================
// Rendering
// ============================================================================

function scheduleRender() {
    if (!state.autoRender) return;
    
    clearTimeout(state.renderDebounce);
    state.renderDebounce = setTimeout(() => {
        console.log('scheduleRender: calling render, state.files keys =', Array.from(state.files.keys()));
        render();
    }, 300);
}

function scheduleLint() {
    if (!state.autoLint) return;
    
    clearTimeout(state.lintDebounce);
    state.lintDebounce = setTimeout(() => {
        lint();
    }, 500);
}

async function lint() {
    if (!state.activeFile || !state.ymxInstance) return;
    
    const fileState = state.files.get(state.activeFile);
    if (!fileState) return;
    
    // Sync editor content to fileState
    if (editor.getModel()) {
        fileState.content = editor.getValue();
    }
    
    try {
        // Build a Map of all .yml files in the virtual filesystem
        const fileMap = new Map();
        for (const [path, f] of state.files) {
            if (path.endsWith('.yml') || path.endsWith('.yaml')) {
                fileMap.set(path, f.content);
            }
        }
        
        // Clear and parse all files (handles _use imports)
        state.ymxInstance.clear();
        state.ymxInstance.parse_map(fileMap);
        
        const result = state.ymxInstance.lint();
        updateLintMarkers(result);
        updateLintPreview(result);
    } catch (error) {
        console.error('Lint error:', error);
    }
}

async function render() {
    if (!state.activeFile || !state.ymxInstance) return;
    
    const fileState = state.files.get(state.activeFile);
    if (!fileState) return;
    
    // Sync editor content to fileState (in case it was set programmatically)
    if (editor.getModel()) {
        fileState.content = editor.getValue();
    }
    
    console.log('render: state.files keys =', Array.from(state.files.keys()));
    
    updateStatus('Rendering...');
    
    try {
        // Build a Map of all .yml files in the virtual filesystem
        const fileMap = new Map();
        for (const [path, f] of state.files) {
            if (path.endsWith('.yml') || path.endsWith('.yaml')) {
                fileMap.set(path, f.content);
            }
        }
        console.log('render: fileMap size =', fileMap.size, 'keys =', Array.from(fileMap.keys()));
        
        // Clear and parse all files (handles _use imports)
        state.ymxInstance.clear();
        state.ymxInstance.parse_map(fileMap);
        
        // Get the main component to render - must be named 'main'
        const components = state.ymxInstance.components();
        const mainComponent = 'main';
        
        // Check if main component exists
        if (!components.includes(mainComponent)) {
            throw new Error('No "main" component found. Please define a "main" component in your YAML to render.');
        }
        
        // Call the component using __get__ method
        const componentFn = state.ymxInstance.__get__.call(state.ymxInstance, mainComponent);
        // The function expects (this, argsArray) - pass empty array for no args
        const result = componentFn(undefined, []);
        
        if (typeof result === 'string' && result.startsWith('YMX Error')) {
            throw new Error(result);
        }
        
        // Render based on view mode
        await renderPreview(result);
        updateStatus('Ready');
    } catch (error) {
        console.error('Render error:', error);
        renderError(error.message);
        updateStatus('Error');
    }
}

function updateLintMarkers(result) {
    if (!state.activeFile) return;
    
    const model = models.get(state.activeFile);
    if (!model) return;
    
    // Clear existing markers
    const markers = model.deltaDecorations([], []);
    
    // Group findings by file - normalize file paths to match active file
    const findingsByFile = new Map();
    for (const finding of result.findings) {
        let filePath = finding.file || state.activeFile;
        // Normalize <inline> to match active file
        if (filePath === '<inline>' && state.activeFile) {
            filePath = state.activeFile;
        }
        if (!findingsByFile.has(filePath)) {
            findingsByFile.set(filePath, []);
        }
        findingsByFile.get(filePath).push(finding);
    }
    
    // Add markers for active file
    const activeFindings = findingsByFile.get(state.activeFile) || [];
    const decorations = [];
    
    for (const finding of activeFindings) {
        const lineNumber = Math.max(1, finding.line);
        const column = Math.max(1, finding.col);
        
        const severityClass = finding.severity === 'Error' ? 'lint-error' : 
                             finding.severity === 'Warning' ? 'lint-warning' : 'lint-info';
        
        decorations.push({
            range: new monaco.Range(lineNumber, column, lineNumber, column + 1),
            options: {
                glyphMarginClassName: severityClass,
                linesDecorationsClassName: severityClass + '-line',
                stickiness: monaco.editor.TrackedRangeStickiness.NeverGrowsWhenTypingAtEdges,
                hoverMessage: { value: `${finding.code}: ${finding.message}` + (finding.help ? `\n${finding.help}` : '') }
            }
        });
    }
    
    model.deltaDecorations(markers, decorations);
}

function updateLintPreview(result) {
    const preview = elements.previewContent;
    const hasErrors = result.findings.some(f => f.severity === 'Error');
    const hasWarnings = result.findings.some(f => f.severity === 'Warning');
    
    if (!hasErrors && !hasWarnings) {
        // No lint issues, don't override preview
        return;
    }
    
    let html = '<div class="lint-preview">';
    
    if (hasErrors) {
        html += '<h3 style="color: var(--error);">Errors</h3>';
    }
    if (hasWarnings) {
        html += '<h3 style="color: var(--warning);">Warnings</h3>';
    }
    
    html += '<ul style="list-style: none; padding: 0;">';
    
    for (const finding of result.findings) {
        const severityColor = finding.severity === 'Error' ? 'var(--error)' : 
                             finding.severity === 'Warning' ? 'var(--warning)' : 'var(--success)';
        
        html += `
            <li style="margin-bottom: 8px; padding: 8px; background: var(--bg-tertiary); border-radius: 4px; border-left: 3px solid ${severityColor};">
                <strong style="color: ${severityColor};">${finding.code}</strong> 
                ${finding.component ? `<span style="color: var(--fg-muted);"> in ${finding.component}</span>` : ''}
                <br>
                <span>${finding.message}</span>
                ${finding.help ? `<br><small style="color: var(--fg-muted);">${finding.help}</small>` : ''}
                ${finding.file ? `<br><small style="color: var(--fg-muted);">${finding.file}:${finding.line}:${finding.col}</small>` : ''}
            </li>
        `;
    }
    
    html += '</ul></div>';
    preview.innerHTML = html;
}

async function renderPreview(result) {
    const preview = elements.previewContent;
    
    switch (state.viewMode) {
        case 'html': {
            // Show raw HTML source
            const html = result.asHtml();
            preview.innerHTML = `
                <pre class="preview-html">${escapeHtml(html)}</pre>
            `;
            break;
        }
        
        case 'json': {
            const json = result.json;
            let formatted;
            try {
                formatted = JSON.stringify(JSON.parse(json), null, 2);
            } catch {
                formatted = json;
            }
            preview.innerHTML = `
                <pre class="preview-json">${escapeHtml(formatted)}</pre>
            `;
            break;
        }
        
        case 'page': {
            // Show rendered HTML (page view)
            const html = result.asHtml();
            preview.innerHTML = `
                <div class="preview-page">
                    ${html}
                </div>
            `;
            break;
        }
        
        case 'pdf': {
            if (!state.ironpressLoaded) {
                preview.innerHTML = `
                    <div class="preview-placeholder">
                        <p>PDF rendering requires <code>ironpress</code> to be loaded.</p>
                        <p>Include it via: <code><script src="https://cdn.jsdelivr.net/npm/ironpress@latest/dist/ironpress.umd.js"></script></code></p>
                    </div>
                `;
                break;
            }
            
            preview.innerHTML = '<div class="loading"><div class="spinner"></div>Generating PDF...</div>';
            
            try {
                const pdfBytes = result.asPdf();
                const blob = new Blob([pdfBytes], { type: 'application/pdf' });
                const url = URL.createObjectURL(blob);
                
                preview.innerHTML = `
                    <div class="preview-pdf">
                        <embed src="${url}" type="application/pdf" />
                    </div>
                `;
            } catch (error) {
                console.error('PDF generation error:', error);
                preview.innerHTML = `
                    <div class="preview-placeholder">
                        <p>Failed to generate PDF: ${error.message}</p>
                    </div>
                `;
            }
            break;
        }
    }
}

function renderError(message) {
    elements.previewContent.innerHTML = `
        <div class="preview-placeholder" style="color: var(--error);">
            <p>Render Error:</p>
            <pre style="margin-top: 8px; font-size: 0.8rem;">${escapeHtml(message)}</pre>
        </div>
    `;
}

function escapeHtml(text) {
    const div = document.createElement('div');
    div.textContent = text;
    return div.innerHTML;
}

// ============================================================================
// Keyboard Shortcuts
// ============================================================================

document.addEventListener('keydown', (e) => {
    // Ctrl/Cmd + S to save (mark as not dirty)
    if ((e.ctrlKey || e.metaKey) && e.key === 's') {
        e.preventDefault();
            e.stopPropagation();
        if (state.activeFile) {
            const fileState = state.files.get(state.activeFile);
            if (fileState && fileState.dirty) {
                fileState.dirty = false;
                updateTabDirty(state.activeFile, false);
                showToast('Saved');
            }
        }
    }
    
    // Ctrl/Cmd + Enter to render
    if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') {
        e.preventDefault();
            e.stopPropagation();
        render();
    }
    
    // Escape to close modal
    if (e.key === 'Escape') {
        hideModal();
        hideContextMenu();
    }
});

// ============================================================================
// Initialization
// ============================================================================

async function init() {
    // Initialize DOM elements first
    initElements();
    
    // Setup all event listeners after elements are initialized
    setupEventListeners();
    
    // Wait for monaco to load
    await waitForMonaco();
    
    // Initialize virtual filesystem
    fs.init();
    
    // Initialize editor
    initEditor();
    
    // Render file tree
    renderFileTree();
    
    // Initialize WASM
    await initWasm();
    
    // Open first file by default
    const firstFile = '/main.yml';
    if (state.files.has(firstFile)) {
        openFile(firstFile);
    }
    
    console.log('YMX Playground initialized');
}

function waitForMonaco() {
    return new Promise((resolve) => {
        if (window.monaco) {
            resolve();
        } else {
            const checkMonaco = setInterval(() => {
                if (window.monaco) {
                    clearInterval(checkMonaco);
                    resolve();
                }
            }, 100);
        }
    });
}

// Handle page unload
window.addEventListener('beforeunload', (e) => {
    const hasDirty = Array.from(state.files.values()).some(f => f.dirty);
    if (hasDirty) {
        e.preventDefault();
            e.stopPropagation();
        e.returnValue = 'You have unsaved changes. Are you sure you want to leave?';
        return e.returnValue;
    }
});

// Start the app
init();