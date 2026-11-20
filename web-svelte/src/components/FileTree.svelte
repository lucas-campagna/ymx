<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import { ChevronRight, File, Folder, FolderOpen } from '@lucide/svelte';
  import { cn } from '$lib/utils';
  import FileTreeItem from './FileTreeItem.svelte';

  export let items: FileTreeItem[] = [];
  export let activePath: string | null = null;
  export let dragSource: string | null = null;

  const dispatch = createEventDispatcher<{
    select: string;
    rename: string;
    delete: string;
    newFile: string;
    newFolder: string;
    dragStart: { path: string; type: 'file' | 'folder' };
    dragOver: string;
    dragLeave: string;
    drop: { source: string; target: string };
    dragEnd: void;
    contextMenu: { event: MouseEvent; path: string; type: 'file' | 'folder' };
  }>();

  let collapsedFolders = new Set<string>();

  function toggleFolder(path: string) {
    if (collapsedFolders.has(path)) {
      collapsedFolders.delete(path);
    } else {
      collapsedFolders.add(path);
    }
    collapsedFolders = new Set(collapsedFolders);
  }

  function isCollapsed(path: string) {
    return collapsedFolders.has(path);
  }

  function getChildren(path: string): FileTreeItem[] {
    const item = findItem(items, path);
    return item?.children || [];
  }

  function findItem(items: FileTreeItem[], path: string): FileTreeItem | undefined {
    for (const item of items) {
      if (item.path === path) return item;
      if (item.children) {
        const found = findItem(item.children, path);
        if (found) return found;
      }
    }
    return undefined;
  }

  function handleClick(path: string, type: 'file' | 'folder', event: MouseEvent) {
    if (type === 'folder') {
      const target = event.target as HTMLElement;
      if (target.closest('.folder-toggle')) {
        toggleFolder(path);
        return;
      }
    }
    dispatch('select', path);
  }

  function handleDoubleClick(path: string) {
    dispatch('rename', path);
  }

  function handleContextMenu(event: MouseEvent, path: string, type: 'file' | 'folder') {
    event.preventDefault();
    event.stopPropagation();
    dispatch('contextMenu', { event, path, type });
  }

  function handleDragStart(event: DragEvent, path: string, type: 'file' | 'folder') {
    dispatch('dragStart', { path, type });
  }

  function handleDragOver(event: DragEvent, path: string) {
    event.preventDefault();
    event.dataTransfer!.dropEffect = 'move';
    if (path !== dragSource) {
      dispatch('dragOver', path);
    }
  }

  function handleDragLeave(event: DragEvent, path: string) {
    event.preventDefault();
    dispatch('dragLeave', path);
  }

  function handleDrop(event: DragEvent, path: string) {
    event.preventDefault();
    if (dragSource && dragSource !== path) {
      dispatch('drop', { source: dragSource, target: path });
    }
    dispatch('dragEnd');
  }

  function handleDragEnd() {
    dispatch('dragEnd');
  }
</script>

{#if items.length > 0}
  <div class="file-tree" role="tree" aria-label="File explorer">
    {#each items as item}
      <FileTreeItem
        item={item}
        depth={0}
        isActive={item.path === activePath}
        isCollapsed={isCollapsed(item.path)}
        children={getChildren(item.path)}
        onClick={handleClick}
        onDoubleClick={handleDoubleClick}
        onContextMenu={handleContextMenu}
        onDragStart={handleDragStart}
        onDragOver={handleDragOver}
        onDragLeave={handleDragLeave}
        onDrop={handleDrop}
        onDragEnd={handleDragEnd}
        onToggle={toggleFolder}
      />
    {/each}
  </div>
{:else}
  <div class="file-tree-empty">
    <p class="text-[var(--fg-muted)] text-sm p-4">No files</p>
  </div>
{/if}