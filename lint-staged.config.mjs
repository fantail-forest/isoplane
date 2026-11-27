// What the pre-commit hook does to staged files. .husky/pre-commit runs lint-staged with this file.
//
// While the tasks run, lint-staged hides every unstaged change and untracked file, so they see the
// staged content alone. It adds to the commit every tracked file a task changes, and restores the
// index and working tree if one fails.
export default {
  '*.rs': 'rustfmt',
  // A new reference page is untracked, so lint-staged does not add it. The reference generator
  // deletes every page it did not write, so the directory holds only its output.
  '*': () => ['task generate', 'git add --all docs/userdocs/docs/reference'],
};
