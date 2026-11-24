# browser — fixed target (git panel, preview mode, file keys, directory tree)

Status: in-progress (opened 2026-11-20)

This file fixes the target of the browser workstream, which covers owner messages 14–16 and 19. It is not
reinterpreted during the loop; a change of scope comes only from a new owner message, quoted here
verbatim.

## Owner messages (verbatim)

Message 14 (2026-11-20):

> 그리고 만약 폴드가 git 폴더면 vscode중에 git graph있는데 그거 /zworkflow:using-dotprd 사용해서 해당 git graph 기능 모두 뽑아서 해당 기능처럼 자유롭게 git 작업할 수 있도록해줘.
> 전체 커핏이랑 브랜치 그래프 볼수 있고 커밋 편집 최대한 자유롭게 할수 있게 해줘. 이런 폴더는 다른쪽은 뷰가 더블 디렉토리 뷰에서 다른쪽에 git 뷰로 변경됨.
>
> 이건 sopen 모드를 2개로 해주는 셈이고 디폴트가 미리보기 뷰.
> - 이 모드일때는 듀얼 디렉토리로 보여주다가 현재 폴더가 git이면 git 패널이 반대편에 나와야하고, 미리보기가 이미지등을 미리 반대편에 적절하게 현재 선택중인 파일의 내용을 미리 보여주도록 해줘.

Message 15 (2026-11-20):

> 그리고 디폴트로 키
> f2 = rename
> f5 = copy
> f6 = move
> ---
> 이거 주요 이러한 쉘들 단축키 리서치해주고 가장 일반적인 기준으로 단축키들 제안해줘.
>
> 그리고 맥북이나 윈도우나 리눅스가 기본적으로 펑션키는 항상 sopen에서 f1 f2 f3.. 펑션키로 동작하도록해줘. (이거 잘 모르겠는데 가능한지 리서치해줘)

Message 16 (2026-11-20):

> 그리도 또 기능
> f10인가 누르면 디렉토리 트리뷰
> 이거 잘 만들어야함.
> 1. 예를들어 디폴트로는 루트가 아니라 홈디렉토리 기준임. 현재 cwd가 홈디렉토리 밑에있으면 홈디렉토리가 루트로 트리로 보여줘야함.
> 만약 홈디렉토리가 아니면 그때 "/" 기준으로 보여줘야하고.
> 2. 디바이스 폴더는 펼쳐주면 안됨. 즉, 펼쳐주는 폴더랑 안펼쳐주는 폴더를 구분할수 있어야함. 예를들어 어떤 폴더 안에 디렉토리가 5개(예)미만이면 디폴트로 펼쳐서 보여주고 아니면 폴더를 닫은채 보여줘야함. (대신에 안에 어떤 느낌인지 폴더랑 파일이 몇개 있고 용량이 얼마 되는지 예측할수 있는 정보는 즉시 보여줘야함)
>
> 이거 리서치해서 영리하게 디렉토리 트리 보여주도록 해줘.

Message 19 (2026-11-24; the machine's name is replaced by its role, as everywhere in this public
repo):

> [the Linux laptop]에서 펑션키가 눌린 상태로 되어 있는데 (f1이면 화면 밝기가 조절됨, 이거 온오프를 어떻게 할수 있음?)
> 이거 디폴트로 xtools에서는 맥북이든 리눅스든 윈도우든 f1키가 실제 F1키 역활하게 해줘 os는 건드리지 말고 여기에서는 항상 f1키는 f1키로 동작하도록 해줘

## Fixed scope

1. **Git panel.** When the browser's current folder is inside a git work tree, local or remote, sopen can show a git panel with:
   - the graph of all commits and branches, including remote branches, tags, stashes and uncommitted changes;
   - commit details with changed files and diffs;
   - every action the Git Graph extension offers. The inventory is in `.prd/research/git-graph-features.md`, and sopen reproduces the UX, not the code.
2. **Free commit editing,** beyond what Git Graph offers:
   - reword, squash/fixup, reorder and drop;
   - edit author and date;
   - amend;
   - continue, skip or abort a rebase, cherry-pick or merge in progress.

   Every history rewrite is confirmed first and leaves a sopen backup ref behind, so it can be undone.
3. **Two browser modes,** switched by one key and shown in the F-key row:
   - **Preview mode** is the default. The panel opposite the active one follows the bar:
     - When the bar is on a **file**, the panel previews it with the built-in viewers: text (wrapped), image, a video frame, an audio waveform, or hex for anything else.
     - When the bar is on a **folder** or `..`, the panel shows the **git panel** if the current folder is inside a git work tree, and otherwise a **directory panel** (the dual-directory view).
   - **Dual-directory mode** is today's two directory panels.
   - In either mode, one key pins the git panel to the opposite side, and Esc unpins it.
   - This reading of message 14 is the workstream's decision, made on 2026-11-20. The owner may change it.
4. **Remote repositories.** Git runs on the host that holds the repository, over the browser's existing ssh connection: the ControlMaster socket from `src/remote/sftp.rs` `connection_args`. Nothing is installed on the remote. Git runs non-interactively: no pager, no editor, no terminal prompt.

5. **File keys (message 15).**
   - F2 renames, F5 copies and F6 moves, between the panels or within one, for local and remote folders.
   - The rest of the default key map follows the most common file-manager convention. The survey and proposal are in `.prd/research/keys.md`.
   - Every file-changing action is confirmed the conventional way.
6. **Function keys act as F1…F12 inside sopen (message 15).**
   - Wherever an OS allows it, sopen switches the keyboard to standard function keys only while sopen is focused, and it restores the setting on focus loss, quit and crash recovery.
   - Wherever an OS does not allow it, a fallback works everywhere: a clickable F-key row and alternate keys. The feasibility research is in `.prd/research/keys.md`.
   - **Message 19** asks for this by default on every OS sopen runs on, without changing the OS, Linux included, where `04-file-keys.md` §6.4 found no per-app mechanism. A feasibility gate (K-L1 in `loop.md`) checks again with a prototype before anything is specified. The workstream's reading, made on 2026-11-24, which the owner may change: macOS's switch, made only while sopen is in front and always restored (04 §6.2–6.3), leaves the OS's setting the owner's. Windows is outside sopen (01 §9).
7. **Directory tree (message 16).**
   - A key opens a directory tree. The key is chosen by the key survey.
   - The root is the home directory when the current folder is under it, otherwise `/`. The same rule applies to the remote user's home on a remote folder.
   - Device folders, other file systems, pseudo file systems and symlinks are never expanded automatically.
   - A folder expands by default only when it is small: fewer than about 5 subfolders, within a row budget.
   - Every collapsed folder shows at once how many folders and files it holds, plus a size estimate. Deeper numbers fill in from a bounded background scan.
   - The rules are in `.prd/research/directory-tree.md`.

## Not in scope

These are not in scope unless a new owner message asks for them:
- creating pull requests in a web forge;
- avatars fetched from the network;
- issue-link settings;
- multi-repo workspaces beyond the repository of the current folder.
