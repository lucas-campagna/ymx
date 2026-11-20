<script lang="ts">
  import { ChevronRight, File, Folder, FolderOpen } from '@lucide/svelte';
  import { cn } from '$lib/utils';

  export let item: FileTreeItem;
  export let depth: number = 0;
  export let isActive: boolean = false;
  export let isCollapsed: boolean = false;
  export let children: FileTreeItem[] = [];

  export let onClick: (path: string, type: 'file' | 'folder', event: MouseEvent) => void;
  export let onDoubleClick: (path: string) => void;
  export let onContextMenu: (event: MouseEvent, path: string, type: 'file' | 'folder') => void;
  export let onDragStart: (event: DragEvent, path: string, type: 'file' | 'folder') => void;
  export let onDragOver: (event: DragEvent, path: string) => void;
  export let onDragLeave: (event: DragEvent, path: string) => void;
  export let onDrop: (event: DragEvent, path: string) => void;
  export let onDragEnd: () => void;
  export let onToggle: (path: string) => void;

  const isFolder = item.type === 'folder';
  const hasChildren = children.length > 0;
</script>

<div
  class="file-tree-item"
  data-path={item.path}
  draggable="true"
  ondragstart={(e) => onDragStart(e, item.path, item.type)}
  ondragover={(e) => onDragOver(e, item.path)}
  ondragleave={(e) => onDragLeave(e, item.path)}
  ondrop={(e) => onDrop(e, item.path)}
  ondragend={onDragEnd}
  oncontextmenu={(e) => onContextMenu(e, item.path, item.type)}
>
  <div
    class={cn(
      'file-tree-item-row flex items-center gap-2 px-2 py-1.5 rounded-md',
      'hover:bg-[var(--bg-hover)] cursor-pointer select-none',
      'transition-colors duration-150',
      isActive && 'bg-[var(--bg-hover)]',
      depth > 0 && 'ml-4'
    )}
    onclick={(e) => onClick(item.path, item.type, e)}
    ondblclick={() => onDoubleClick(item.path)}
    draggable="true"
    role="treeitem"
    aria-selected={isActive}
    aria-expanded={isFolder ? !isCollapsed : undefined}
    aria-level={1}
  >
    {#if isFolder}
      <button
        class="folder-toggle flex items-center justify-center w-5 h-5 rounded hover:bg-[var(--bg-hover)] transition-colors"
        onclick={(e) => { e.stopPropagation(); onToggle(item.path); }}
        aria-label={isCollapsed ? 'Expand folder' : 'Collapse folder'}
      >
        {#if isCollapsed}
          <ChevronRight class="w-4 h-4 text-[var(--fg-muted)]" />
        {:else}
          <ChevronRight class="w-4 h-4 text-[var(--fg-muted)] rotate-90" />
        {/if}
      </button>
    {:else}
      <div class="w-5" />
    {/if}

    <span class="flex items-center gap-2 flex-1 min-w-0">
      {#if isFolder}
        {#if isCollapsed}
          <Folder class="w-4 h-4 text-[var(--warning)]" />
        {:else}
          <FolderOpen class="w-4 h-4 text-[var(--warning)]" />
        {/if}
      {:else}
        <File class="w-4 h-4 text-[var(--fg-muted)]" />
      {/if}
      <span class="truncate text-sm">{item.name}</span>
    </span>
  </div>

  {#if isFolder && !isCollapsed && children.length > 0}
    <div class="file-tree-children border-l border-[var(--border)] pl-2 mt-1">
      {#each children as child}
        <svelte:self
          item={child}
          depth={depth + 1}
          isActive={false}
          isCollapsed={false}
          children={[]}
          onClick={onClick}
          onDoubleClick={onDoubleClick}
          onContextMenu={onContextMenu}
          onDragStart={onDragStart}
          onDragOver={onDragOver}
          onDragLeave={onDragLeave}
          onDrop={onDrop}
          onDragEnd={onDragEnd}
          onToggle={onToggle}
        />
      {/each}
    </div>
  {/if}
</div>