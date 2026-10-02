# Keyboard Shortcuts

Sik keeps a **workspace** for each folder you open: its files, its terminals and its Claude Code session. A git repo's **worktrees** are workspaces too, folded under the repo in the workspaces column, so several branches can be open side by side. Terminals live in an agent, on this machine or on a server: closing Sik doesn't stop them, and they come back as they were.

From any terminal, `sik <folder or file>` opens it in Sik.

The shortcuts below are the current ones: change them in Settings → Keyboard Shortcuts. Those marked *(editor)* can't be changed.

## Getting around

| Shortcut | Does |
| --- | --- |
| {{OpenCommandPalette}} or {{ShowShortcuts}} | Command palette: run any command by name |
| {{OpenFileFinder}} | Go to a file by name |
| {{OpenTaskPicker}} | Go to a workspace, on any server |
| {{PreviousTask}} | Back to the previous workspace (again: the one before) |
| {{OpenFolder}} | Open a folder |
| {{OpenRemoteFolder}} | Open a folder on a server |
| {{OpenRecent}} | Open a recent folder |
| {{OpenSettings}} | Settings |

## Workspaces and worktrees

| Shortcut | Does |
| --- | --- |
| {{ToggleTasks}} | Show or hide the workspaces column |
| {{NewTask}} | New worktree in the current repo |

Right-click a workspace to hide it, remove it from the list (nothing on disk is touched) or delete a worktree. Drag them to reorder: a repo moves with its worktrees.

## Files and tabs

| Shortcut | Does |
| --- | --- |
| {{Save}} | Save |
| {{CloseTab}} | Close the tab (or the terminal) |
| {{CloseAllTabs}} | Close all tabs |
| {{NextTab}} | Next tab |
| {{PrevTab}} | Previous tab |
| {{SplitEditorRight}} | Split the editor to the right |
| {{SplitEditorDown}} | Split the editor down |

## Editing

| Shortcut | Does |
| --- | --- |
| {{Undo}} | Undo *(editor)* |
| {{Redo}} | Redo *(editor)* |
| {{MoveLineUp}} / {{MoveLineDown}} | Move the line, or the selected lines, up or down *(editor)* |
| {{DuplicateLineUp}} / {{DuplicateLineDown}} | Copy the line above or below *(editor)* |
| {{Indent}} / {{Outdent}} | Indent or outdent the lines *(editor)* |
| {{FormatDocument}} | Format the file (the repo's `.sik/format`, else its language server) |
| {{ToggleWordWrap}} | Wrap long lines |

## Multiple cursors

Each cursor types, deletes and moves at the same time as the others. {{key:escape}} goes back to one.

| Shortcut | Does |
| --- | --- |
| {{SelectNextOccurrence}} | Select the word under the cursor; again, add its next occurrence *(editor)* |
| {{AddCursorAbove}} / {{AddCursorBelow}} | Add a cursor on the line above or below *(editor)* |
| {{alt}}-click | Add a cursor where you click *(editor)* |
| {{alt}}-{{shift}}-drag | Select a block, a column across lines *(editor)* |

To rename something in a few places: put the cursor on it, press {{SelectNextOccurrence}} once per occurrence you want, and type.

## Code

| Shortcut | Does |
| --- | --- |
| {{GoToDefinition}} | Go to the definition |
| {{FindReferences}} | Find the references, in the References panel |
| {{GoToLine}} | Go to a line (`line` or `line:column`) |
| {{GoToSymbol}} | Go to a function, class or method of the file (Markdown: a heading) |
| {{GoToWorkspaceSymbol}} | Go to a function, class or method of the whole workspace |
| {{NavigateBack}} / {{NavigateForward}} | Back and forward through the places you jumped to |
| {{ToggleMarkdownSource}} | Markdown: show the source or the preview |
| {{OpenPreviewToSide}} | Markdown: open the preview to the side |

## Debugging

`.sik/debug.json` says how to start the workspace's program; see the debugger's documentation.

| Shortcut | Does |
| --- | --- |
| {{DebugContinue}} | Start debugging, or continue the stopped program |
| {{DebugStop}} | Stop debugging |
| {{DebugRestart}} | Restart |
| {{DebugPause}} | Pause: the next code that runs stops |
| {{ToggleBreakpoint}} | Toggle a breakpoint on the cursor's line (or click the gutter; right-click it for a condition) |
| {{StepOver}} / {{StepInto}} / {{StepOut}} | Step over, into, out |
| {{RunToCursor}} | Run to the cursor's line |
| {{SetNextStatement}} | Make the cursor's line the next one to run |
| {{ToggleDebugPanel}} | Show or hide the debugger |

## Search

| Shortcut | Does |
| --- | --- |
| {{Search}} | Find in the file *(editor)* |
| {{Replace}} | Replace in the file *(editor)* |
| {{ShowSearch}} | Search (and replace) in the whole workspace |
| {{NextResult}} / {{PrevResult}} | Next or previous result, of a search or the references |

## Side panel

| Shortcut | Does |
| --- | --- |
| {{ToggleSidePanel}} | Show or hide the side panel |
| {{ShowFiles}} | Files |
| {{CollapseFileTree}} | Collapse all the folders |
| {{ShowChanges}} | Changes: what isn't committed, and the history |
| {{ShowReferences}} | References |
| {{ToggleActivityBar}} | Show or hide the activity bar |

Each panel has an icon in the activity bar, on the left: a click shows or hides it wherever it's placed. Drag an icon up or down to reorder the bar, or onto a panel to place it there, as with its tab.

## Terminals

| Shortcut | Does |
| --- | --- |
| {{ToggleTerminals}} | Show or hide the terminals |
| {{MaximizeTerminals}} | Terminals over the whole window, or back |
| {{NewTerminal}} | New terminal |
| {{SplitRight}} / {{SplitDown}} | Split the terminal to the right or down |
| {{FocusPaneLeft}} {{FocusPaneRight}} {{FocusPaneUp}} {{FocusPaneDown}} | Move to the terminal on that side |

Drag a terminal's tab onto the edge of another to split them; drag a pane's title back to the tab bar to separate it. A `file:line` in a terminal opens with a click.

## The app

| Shortcut | Does |
| --- | --- |
| {{Quit}} | Quit (it asks about unsaved files) |
