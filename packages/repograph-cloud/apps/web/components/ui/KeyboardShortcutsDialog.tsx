'use client'

import { Keyboard } from 'lucide-react'

import { Dialog, DialogContent, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { formatShortcut, GLOBAL_SHORTCUTS, REPOSITORY_SHORTCUTS,SEARCH_SHORTCUTS } from '@/lib/hooks/use-keyboard-shortcuts'

interface KeyboardShortcutsDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

export function KeyboardShortcutsDialog({ open, onOpenChange }: KeyboardShortcutsDialogProps) {
  const shortcuts = [
    ...GLOBAL_SHORTCUTS,
    ...SEARCH_SHORTCUTS,
    ...REPOSITORY_SHORTCUTS
  ]

  // Group shortcuts by category
  const groupedShortcuts = shortcuts.reduce((acc, shortcut) => {
    const category = shortcut.category || 'Other'
    if (!acc[category]) {
      acc[category] = []
    }
    acc[category].push(shortcut)
    return acc
  }, {} as Record<string, typeof shortcuts>)

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl max-h-[80vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Keyboard className="h-5 w-5" />
            Keyboard Shortcuts
          </DialogTitle>
        </DialogHeader>

        <div className="space-y-6 mt-4">
          {Object.entries(groupedShortcuts).map(([category, categoryShortcuts]) => (
            <div key={category}>
              <h3 className="text-sm font-semibold text-slate-900 dark:text-white mb-3">
                {category}
              </h3>
              <div className="space-y-2">
                {categoryShortcuts.map((shortcut, index) => (
                  <div
                    key={index}
                    className="flex items-center justify-between py-2 px-3 rounded-lg hover:bg-slate-50 dark:hover:bg-slate-800"
                  >
                    <span className="text-sm text-slate-600 dark:text-slate-400">
                      {shortcut.description}
                    </span>
                    <kbd className="px-2 py-1 text-xs font-semibold text-slate-800 dark:text-slate-200 bg-slate-100 dark:bg-slate-700 border border-slate-200 dark:border-slate-600 rounded">
                      {formatShortcut(shortcut)}
                    </kbd>
                  </div>
                ))}
              </div>
            </div>
          ))}
        </div>

        <div className="mt-6 pt-4 border-t border-slate-200 dark:border-slate-800">
          <p className="text-xs text-slate-500 dark:text-slate-400 text-center">
            Press <kbd className="px-1.5 py-0.5 text-xs font-semibold bg-slate-100 dark:bg-slate-700 border border-slate-200 dark:border-slate-600 rounded">Escape</kbd> to close
          </p>
        </div>
      </DialogContent>
    </Dialog>
  )
}
