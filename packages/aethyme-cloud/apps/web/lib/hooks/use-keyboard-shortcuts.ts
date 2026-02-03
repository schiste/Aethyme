import { useCallback,useEffect } from 'react'

export interface KeyboardShortcut {
  key: string
  ctrl?: boolean
  shift?: boolean
  alt?: boolean
  meta?: boolean
  handler: (event: KeyboardEvent) => void
  description: string
  category?: string
}

interface UseKeyboardShortcutsOptions {
  shortcuts: KeyboardShortcut[]
  enabled?: boolean
}

/**
 * Hook for managing keyboard shortcuts
 *
 * @example
 * useKeyboardShortcuts({
 *   shortcuts: [
 *     {
 *       key: 'k',
 *       meta: true,
 *       handler: () => focusSearch(),
 *       description: 'Focus search',
 *       category: 'Navigation'
 *     }
 *   ]
 * })
 */
export function useKeyboardShortcuts({
  shortcuts,
  enabled = true
}: UseKeyboardShortcutsOptions) {
  const handleKeyDown = useCallback(
    (event: KeyboardEvent) => {
      if (!enabled) return

      // Don't trigger shortcuts when typing in inputs (unless explicitly allowed)
      const target = event.target as HTMLElement
      const isInput = ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName)
      const isContentEditable = target.isContentEditable

      for (const shortcut of shortcuts) {
        const keyMatch = event.key.toLowerCase() === shortcut.key.toLowerCase()
        const ctrlMatch = shortcut.ctrl ? event.ctrlKey : true
        const shiftMatch = shortcut.shift ? event.shiftKey : !event.shiftKey
        const altMatch = shortcut.alt ? event.altKey : !event.altKey
        const metaMatch = shortcut.meta ? event.metaKey : !event.metaKey

        // Check if modifier keys match
        const modifiersMatch = ctrlMatch && shiftMatch && altMatch && metaMatch

        if (keyMatch && modifiersMatch) {
          // Some shortcuts should work even in inputs (like Escape)
          const allowInInput = ['escape', 'esc'].includes(shortcut.key.toLowerCase())

          if (isInput || isContentEditable) {
            if (!allowInInput) continue
          }

          event.preventDefault()
          shortcut.handler(event)
          break
        }
      }
    },
    [shortcuts, enabled]
  )

  useEffect(() => {
    if (!enabled) return

    document.addEventListener('keydown', handleKeyDown)
    return () => document.removeEventListener('keydown', handleKeyDown)
  }, [handleKeyDown, enabled])
}

/**
 * Platform-aware keyboard shortcut formatter
 */
export function formatShortcut(shortcut: Omit<KeyboardShortcut, 'handler'>): string {
  const isMac = typeof navigator !== 'undefined' && navigator.platform.toUpperCase().indexOf('MAC') >= 0
  const parts: string[] = []

  if (shortcut.ctrl) parts.push(isMac ? '⌃' : 'Ctrl')
  if (shortcut.alt) parts.push(isMac ? '⌥' : 'Alt')
  if (shortcut.shift) parts.push(isMac ? '⇧' : 'Shift')
  if (shortcut.meta) parts.push(isMac ? '⌘' : 'Ctrl')

  parts.push(shortcut.key.toUpperCase())

  return parts.join(isMac ? '' : '+')
}

/**
 * Global keyboard shortcuts for the application
 */
export const GLOBAL_SHORTCUTS: Omit<KeyboardShortcut, 'handler'>[] = [
  {
    key: 'k',
    meta: true,
    description: 'Focus search',
    category: 'Navigation'
  },
  {
    key: '/',
    meta: true,
    description: 'Show keyboard shortcuts',
    category: 'Help'
  },
  {
    key: 'escape',
    description: 'Close dialog / Clear search',
    category: 'General'
  },
  {
    key: 'b',
    meta: true,
    description: 'Toggle sidebar',
    category: 'Navigation'
  },
  {
    key: 'f',
    meta: true,
    shift: true,
    description: 'Advanced search',
    category: 'Search'
  }
]

export const SEARCH_SHORTCUTS: Omit<KeyboardShortcut, 'handler'>[] = [
  {
    key: 'ArrowDown',
    description: 'Next result',
    category: 'Search'
  },
  {
    key: 'ArrowUp',
    description: 'Previous result',
    category: 'Search'
  },
  {
    key: 'Enter',
    description: 'Open selected result',
    category: 'Search'
  }
]

export const REPOSITORY_SHORTCUTS: Omit<KeyboardShortcut, 'handler'>[] = [
  {
    key: 'f',
    meta: true,
    description: 'Focus file tree search',
    category: 'Repository'
  },
  {
    key: 'ArrowUp',
    description: 'Previous file',
    category: 'Repository'
  },
  {
    key: 'ArrowDown',
    description: 'Next file',
    category: 'Repository'
  },
  {
    key: 'ArrowLeft',
    description: 'Collapse folder',
    category: 'Repository'
  },
  {
    key: 'ArrowRight',
    description: 'Expand folder',
    category: 'Repository'
  },
  {
    key: 'Enter',
    description: 'Open selected file',
    category: 'Repository'
  }
]
