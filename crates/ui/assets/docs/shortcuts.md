# Keyboard Shortcuts

Den keeps a **workspace** for each folder you open: its files, its terminals and its Claude Code session. A git repo's **worktrees** are workspaces too, listed with the repo in the Workspaces panel, so several branches can be open side by side. Terminals live in an agent, on this machine or on a server: closing Den doesn't stop them, and they come back as they were.

From any terminal, `den <folder or file>` opens it in Den.

The shortcuts below are the current ones: change them in Settings → Keyboard Shortcuts. Those marked *(editor)* can't be changed.

## Getting around

| Shortcut | Does |
| --- | --- |
| {{OpenCommandPalette}} or {{ShowShortcuts}} | Command palette: run any command by name |
| {{OpenFileFinder}} | Go to a file by name |
| {{OpenTaskPicker}} | Go to a workspace, on any server |
| {{PreviousTask}} | Switch workspace, as Cmd-Tab: the previous one, then those with an agent, waiting ones first; holding Cmd, E again goes further, Shift-E back; let go to enter |
| {{NextActiveTask}} | Go straight into the next workspace with an agent; while Cmd is down the workspaces show where it is (over the window if hidden) and, at the bottom, its notes and agents |
| {{NextTask}} | Go straight into the next workspace |
| {{OpenFolder}} | Open a folder |
| {{OpenRemoteFolder}} | Open a folder on a server |
| {{OpenRecent}} | Open a recent folder |
| {{OpenSettings}} | Settings |

## Workspaces and worktrees

| Shortcut | Does |
| --- | --- |
| {{ToggleTasks}} | Show or hide the workspaces |
| {{NewTask}} | New worktree in the current repo |

Right-click a workspace to hide it, remove it from the list (nothing on disk is touched) or delete a worktree. Drag them to reorder: a repo moves with its worktrees.

## Files and tabs

| Shortcut | Does |
| --- | --- |
| {{NewFile}} | New file, in the folder of the file open (the workspace's when none is): its name is typed in the files panel |
| {{Save}} | Save |
| {{CloseTab}} | Close the tab (or the terminal) |
| {{CloseAllTabs}} | Close all tabs |
| {{NextTab}} | Next tab |
| {{PrevTab}} | Previous tab |
| {{SplitEditorRight}} | Split the editor to the right |
| {{SplitEditorDown}} | Split the editor down |

## The files panel

| Shortcut | Does |
| --- | --- |
| {{key:secondary-c}} / {{key:secondary-x}} / {{key:secondary-v}} | Copy, cut, paste: into the selected folder, or the selected file's |
| {{key:enter}} | Rename |
| {{key:secondary-backspace}} | Move to the Trash |
| {{key:secondary-z}} | Undo the last move, rename, copy, new file or trip to the Trash |
| {{key:shift-up}} / {{key:shift-down}} | Select the rows above or below too |
| {{key:secondary-a}} | Select every row shown |
| {{key:escape}} | Back to one row, and nothing cut |
| {{RefreshFiles}} | Read the folders again (also in the panel's menu) |

{{secondary}}-click adds a row to the selection or takes it out, {{shift}}-click selects up to it; whatever is done to a selected row is done to all of them. Drag rows onto a folder to move them, holding {{alt}} to copy them; held over a closed folder, it opens. Copying never replaces anything: pasted where it is, or with Duplicate in the menu, the copy is named "a copy.txt". Files dropped from Finder or the Explorer, or copied there and pasted, are copied in, to a server too.

## Editing

| Shortcut | Does |
| --- | --- |
| {{Undo}} | Undo *(editor)* |
| {{Redo}} | Redo *(editor)* |
| {{MoveLineUp}} / {{MoveLineDown}} | Move the line, or the selected lines, up or down *(editor)* |
| {{DuplicateLineUp}} / {{DuplicateLineDown}} | Copy the line above or below *(editor)* |
| {{Indent}} / {{Outdent}} | Indent or outdent the lines *(editor)* |
| {{FormatDocument}} | Format the file (the repo's `.den/format`, else its language server) |
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

`.den/debug.json` says how to start the workspace's program; see the debugger's documentation.

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
| {{ToggleDebugPanel}} | Show or hide the debugger's tab: its toolbar, the call stack, the variables, the watches, the breakpoints and the console |

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
| {{ToggleSidePanel}} | Show or hide the side column |
| {{ShowFiles}} | Files |
| {{CollapseFileTree}} | Collapse all the folders |
| {{ShowChanges}} | Changes: what isn't committed |
| {{ShowHistory}} | The History tab: the current branch's commits (All Branches: every branch's) with their graph, the selected one's diff and its files; its search looks in the messages, the paths or the content |
| {{ShowReferences}} | References |
| {{ShowOutline}} | Outline: the classes, functions, constants… of the file in front, not what's inside the functions |

The activity bar, on the left, has an icon for each group of the side column: the explorer (workspaces, agents, files, outline), search and source control; after them, History's, which opens the History tab or closes it. A click shows its group, or closes the column if it's the one showing. Drag an icon up or down to reorder the bar. A click on a panel's header folds it; drag it onto another's header to put it above, or its lower edge to size it. Any panel goes in any icon's column: drag its header onto the icon, or the icon onto the column to bring its panels. Right-click a panel to hide it or give it an icon of its own; right-click the bar for every panel, to bring it to the column showing or take it off, and for History. In the History tab, ↑ and ↓ go through the commits. View > Reset Layout puts everything back where it starts. The side column stays as it is from one workspace to another; each keeps whether its terminals show.

## Terminals

| Shortcut | Does |
| --- | --- |
| {{ToggleTerminals}} | Show or hide the terminals |
| {{MaximizeTerminals}} | Terminals over the whole window, or back |
| {{MoveTerminals}} | Terminals under the code, or back on its right |
| {{ToggleTerminalMode}} | Terminal Mode: only the terminals, with the workspaces and the agents beside them; again, back |
| {{ToggleNotes}} | The workspace's notes, a tab at the far end of the terminals', to write what's next in it; again, back to the terminals |
| {{NewTerminal}} | New terminal |
| {{SplitRight}} / {{SplitDown}} | Split the terminal to the right or down |
| {{FocusPaneLeft}} {{FocusPaneRight}} {{FocusPaneUp}} {{FocusPaneDown}} | Move to the terminal on that side |
| {{TerminalFind}} | Find in the terminal, its history included: {{key:enter}} goes up, {{key:shift-enter}} down |

In Terminal Mode, {{ToggleSidePanel}} shows or hides the workspaces and the agents; whatever needs the code (a file opened, a `file:line` clicked, a side panel, {{ToggleTerminals}}) takes the window back to the code. `den -t` opens a window in it.

Drag a terminal's tab onto the edge of another to split them; drag a pane's title back to the tab bar to separate it. A `file:line` in a terminal opens with a click.

## The app

| Shortcut | Does |
| --- | --- |
| {{Quit}} | Quit (it asks about unsaved files) |
| {{ReloadWindow}} | Reload Window: quits as {{Quit}} does and starts again; the terminals go on, in the agent |
