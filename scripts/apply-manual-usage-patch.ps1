$ErrorActionPreference = 'Stop'

$hook = 'src/hooks/useAccounts.ts'
$app = 'src/App.tsx'

$hookText = (Get-Content $hook -Raw).Replace("`r`n", "`n")
$hookText = $hookText.Replace(
  "        const accountList = await loadAccounts();`n        await refreshUsage(accountList);",
  "        await loadAccounts();"
)
$hookText = $hookText.Replace('[loadAccounts, refreshUsage]', '[loadAccounts]')
$hookText = [regex]::Replace(
  $hookText,
  '(?s)  useEffect\(\(\) => \{\n    loadAccounts\(\)\.then\(\(accountList\) => refreshUsage\(accountList\)\);.*?    return \(\) => clearInterval\(interval\);\n  \}, \[loadAccounts\]\);',
  "  useEffect(() => {`n    void loadAccounts();`n  }, [loadAccounts]);"
)
Set-Content -Path $hook -Value $hookText -Encoding utf8 -NoNewline

$appText = (Get-Content $app -Raw).Replace("`r`n", "`n")
$appText = $appText.Replace(
  'import { AccountCard, AddAccountModal, UpdateChecker } from "./components";',
  'import { AccountCard, AddAccountModal } from "./components";'
)
$appText = $appText.Replace(
  "      const accountList = await loadAccounts();`n      await refreshUsage(accountList);",
  "      await loadAccounts();"
)
$appText = $appText.Replace("      <UpdateChecker />`n`n", '')
Set-Content -Path $app -Value $appText -Encoding utf8 -NoNewline

$hookText = Get-Content $hook -Raw
$appText = Get-Content $app -Raw

if ($hookText -match '60000|refreshUsage\(accountList\)|loadAccounts\(\)\.then') {
  throw 'Automatic usage refresh remains.'
}
if ($appText -match 'UpdateChecker|refreshUsage\(accountList\)') {
  throw 'Automatic update or usage refresh remains in App.'
}
if ($hookText -notmatch 'const refreshSingleUsage' -or $appText -notmatch 'await refreshUsage\(\)') {
  throw 'Manual refresh path is missing.'
}
if ($hookText -notmatch 'warmup_account' -or $hookText -notmatch 'warmup_all_accounts') {
  throw 'Warmup path changed unexpectedly.'
}

Write-Host 'Custom source changes applied and verified.'
