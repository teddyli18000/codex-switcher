$ErrorActionPreference = 'Stop'

$hook = 'src/hooks/useAccounts.ts'
$app = 'src/App.tsx'

$hookText = (Get-Content $hook -Raw).Replace("`r`n", "`n")
$hookText = $hookText.Replace(
  "        const accountList = await loadAccounts();`n        await refreshUsage(accountList);",
  "        await loadAccounts();"
)
$hookText = [regex]::Replace(
  $hookText,
  '(?s)  useEffect\(\(\) => \{\n    loadAccounts\(\)\.then\(\(accountList\) => refreshUsage\(accountList\)\);.*?    return \(\) => clearInterval\(interval\);\n  \}, \[loadAccounts, refreshUsage\]\);',
  "  useEffect(() => {`n    void loadAccounts();`n  }, [loadAccounts]);"
)
Set-Content -Path $hook -Value $hookText -Encoding utf8 -NoNewline

$appText = (Get-Content $app -Raw).Replace("`r`n", "`n")
$appText = $appText.Replace(
  "      const accountList = await loadAccounts();`n      await refreshUsage(accountList);",
  "      await loadAccounts();"
)
Set-Content -Path $app -Value $appText -Encoding utf8 -NoNewline

$hookText = Get-Content $hook -Raw
$appText = Get-Content $app -Raw

if ($hookText -match '60000|refreshUsage\(accountList\)|loadAccounts\(\)\.then') {
  throw 'Automatic usage refresh remains in account hook.'
}
if ($appText -match 'refreshUsage\(accountList\)') {
  throw 'Automatic usage refresh remains in app import flow.'
}
if ($hookText -notmatch 'const refreshSingleUsage' -or $appText -notmatch 'await refreshUsage\(\)') {
  throw 'Manual refresh path is missing.'
}
if ($hookText -notmatch 'warmup_account' -or $hookText -notmatch 'warmup_all_accounts') {
  throw 'Warmup path changed unexpectedly.'
}

Write-Host 'Minimal custom source changes applied and verified.'
