# Normalize the one source block that uses six-space indentation.
$path = 'src/hooks/useAccounts.ts'
$text = (Get-Content $path -Raw).Replace("`r`n", "`n")
$text = $text.Replace(
  "      const accountList = await loadAccounts();`n      await refreshUsage(accountList);",
  "      await loadAccounts();"
)
Set-Content -Path $path -Value $text -Encoding utf8 -NoNewline
