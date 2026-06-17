$ErrorActionPreference = 'Stop'

function Replace-Regex {
  param(
    [Parameter(Mandatory = $true)][string]$Path,
    [Parameter(Mandatory = $true)][string]$Pattern,
    [Parameter(Mandatory = $true)][string]$Replacement,
    [Parameter(Mandatory = $true)][int]$ExpectedCount
  )

  $text = Get-Content $Path -Raw
  $regex = [regex]::new($Pattern, [System.Text.RegularExpressions.RegexOptions]::Multiline)
  $count = $regex.Matches($text).Count
  if ($count -ne $ExpectedCount) {
    throw "Unexpected source in $Path: expected $ExpectedCount match(es), found $count"
  }
  $updated = $regex.Replace($text, $Replacement)
  Set-Content -Path $Path -Value $updated -Encoding utf8 -NoNewline
}

$hook = 'src/hooks/useAccounts.ts'
$app = 'src/App.tsx'

Replace-Regex $hook '        const accountList = await loadAccounts\(\);\r?\n        await refreshUsage\(accountList\);' '        await loadAccounts();' 4
Replace-Regex $hook 'loadAccounts, refreshUsage' 'loadAccounts' 5
Replace-Regex $hook '  useEffect\(\(\) => \{\r?\n    loadAccounts\(\)\.then\(\(accountList\) => refreshUsage\(accountList\)\);\r?\n\s*\r?\n    // Auto-refresh usage every 60 seconds \(same as official Codex CLI\)\r?\n    const interval = setInterval\(\(\) => \{\r?\n      refreshUsage\(\)\.catch\(\(\) => \{\}\);\r?\n    \}, 60000\);\r?\n\s*\r?\n    return \(\) => clearInterval\(interval\);\r?\n  \}, \[loadAccounts\]\);' "  useEffect(() => {`n    void loadAccounts();`n  }, [loadAccounts]);" 1

Replace-Regex $app 'import \{ AccountCard, AddAccountModal, UpdateChecker \} from "\./components";' 'import { AccountCard, AddAccountModal } from "./components";' 1
Replace-Regex $app '      const accountList = await loadAccounts\(\);\r?\n      await refreshUsage\(accountList\);' '      await loadAccounts();' 1
Replace-Regex $app '      <UpdateChecker />\r?\n\r?\n' '' 1

$hookText = Get-Content $hook -Raw
$appText = Get-Content $app -Raw
if ($hookText -match '60000|refreshUsage\(accountList\)|loadAccounts\(\)\.then') { throw 'Automatic usage refresh remains.' }
if ($appText -match 'UpdateChecker|refreshUsage\(accountList\)') { throw 'Automatic update or usage refresh remains in App.' }
if ($hookText -notmatch 'const refreshSingleUsage' -or $appText -notmatch 'await refreshUsage\(\)') { throw 'Manual refresh path is missing.' }
if ($hookText -notmatch 'warmup_account' -or $hookText -notmatch 'warmup_all_accounts') { throw 'Warmup path changed unexpectedly.' }

Write-Host 'Custom patch applied successfully.'
