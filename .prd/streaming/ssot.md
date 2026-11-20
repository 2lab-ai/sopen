# streaming — fixed target

Status: in-progress (opened 2026-11-20)

This file fixes the target of the streaming workstream (owner message 17). It is not reinterpreted
during the loop; a change of scope comes only from a new owner message, quoted here verbatim.

## Owner message (verbatim)

Message 17 (2026-11-20):

> 그리고 파일뷰어 이미지 뷰어 영상 모두 지금 병신처럼 전체를 다 받아서 처리하는데 이거 모두 스트리밍 처리해줘. 예를들어 24기가 짜리 모델 파일을 시팔 24기가 다 받아서 보여주는데 말이 안되잖아. 모두 스트리밍 처리해줘 이건 충분히 리서치해서 해줘
>
> 예를들어 그냥 파일 뷰어면 보여줄수 있을만큼만 먼저 받고 먼저 화면 보여주고 그 다음에 시퀀셜로 볼경우에 몇개의 뷰를 미리 로딩해서 앞뒤로 먼저 로딩해서 보여주고 (전체 다운로드 X) 멀리 이동하면 이거는 시간이 좀 걸리는데 그렇게 처리해줘. 예를들어 유투브에서 동영상 스트리밍한다고 생각하고 리서치해서 파일도 바이너리 파일도 음악, 영상 모두 그렇게 처리해줘.
>
> 그리고 그냥 뷰어가 아니라 만약 편집할 경우는 다른데 이것은 별도로 리서치해서 처리해줘. 편집할경우는 그 편집된 부분은 전부 현재 로컬에 해당 내역이 모두 메모리 혹은 파일로 임시 저장해야하는거고 원하면 save as 로 로컬에 저장할 수 있게 해줘. (디폴트는 원격 편집 저장임)

## Fixed scope

1. **Viewing streams.** Opening a remote file never downloads the whole file before showing it. This covers every viewer: the text viewer, the hex/binary viewer, the image viewer, and audio and video playback.
   - A viewer fetches what its first screen needs and shows it.
   - It then prefetches a bounded window ahead of and behind the position, like a video player's buffer, so sequential viewing stays smooth.
   - A far jump fetches the new position on demand. It may take a moment and shows that it is loading.
   - Nothing downloads the whole file unless an action needs the whole file: export, a stems run, an image decode that needs every byte, or Save As local. Even then it streams with progress and can be cancelled.
   - The design comes from research (`.prd/research/streaming-view.md`), not from guesses.
2. **Editing a streamed file** is researched separately (`.prd/research/streaming-edit.md`).
   - Every edit lives locally, in memory or a local temp file, until it is saved.
   - Save writes back to the remote file by default, keeping §5's safety: a conflict check and a backup, never silent.
   - Save As can write the edited result to a local path instead.
   - Saving a small edit to a huge remote file must not re-upload or re-download the whole file when a cheaper safe path exists.
3. **Acceptance example from the owner:** a remote 24 GB model file opens in sopen and shows its first screen without fetching more than a bounded amount, and memory and disk use stay bounded while scrolling.

## Decisions made by the workstream (told to the owner 2026-11-20; reversible)

The edit research ([`../research/streaming-edit.md`](../research/streaming-edit.md) §7) marked these
"the owner's call". The orchestrator decided them on 2026-11-20 and told the owner in the session.
Each can be reversed, and none is the owner's words. A new owner message overrides any of them.

1. **Write in place.** Offered only from an explicit dialog when no atomic route exists, and never as
   a fallback. It is not atomic, and it says so ([`../07-streaming-edit.md`](../07-streaming-edit.md)
   §4.4).
2. **Load as edits.** The hex backup is a local undo file of the replaced bytes, restored only by
   loading it as edits in the hex window and saving them with every check (07 §5.1).
3. **The remote hard-link backup.** A media Save of a remote original keeps the original's inode as a
   hidden hard link next to it, named in the result line and disclosed as still changeable (07 §5.3).
4. **Continue above the image decode budget.** Above 512 MiB decoded, the image window shows the memory
   needed and lets the owner continue; it does not refuse
   ([`../06-streaming.md`](../06-streaming.md) §7.3). This applies to local images too.

Other choices that go beyond message 17's words are labelled as the workstream's where they are made:
- cancelling export and Save to… (06 §7.4, §7.5; item 1's "can be cancelled" is this file's wording,
  not the owner's);
- the durable journal and save reconciliation (07 §3, §3.1);
- bounding the text index for local files (06 §7.2).
