$ErrorActionPreference = 'Stop'

function Replace-Exact {
  param(
    [Parameter(Mandatory = $true)][string]$Path,
    [Parameter(Mandatory = $true)][string]$Old,
    [Parameter(Mandatory = $true)][string]$New,
    [Parameter(Mandatory = $true)][string]$Description
  )

  $content = Get-Content $Path -Raw
  $count = ([regex]::Matches($content, [regex]::Escape($Old))).Count
  if ($count -ne 1) {
    throw "$Description: expected exactly one match in $Path, found $count"
  }
  $content = $content.Replace($Old, $New)
  Set-Content -Path $Path -Value $content -Encoding utf8 -NoNewline
}

$hook = 'src/hooks/useAccounts.ts'
$app = 'src/App.tsx'

Replace-Exact -Path $hook -Description 'Disable usage refresh after auth.json import' -Old @'
        const accountList = await loadAccounts();
        await refreshUsage(accountList);
      } catch (err) {
        throw err;
      }
    },
    [loadAccounts, refreshUsage]
  );

  const startOAuthLogin
'@ -New @'
        await loadAccounts();
      } catch (err) {
        throw err;
      }
    },
    [loadAccounts]
  );

  const startOAuthLogin
'@

Replace-Exact -Path $hook -Description 'Disable usage refresh after OAuth login' -Old @'
      const account = await invokeBackend<AccountInfo>("complete_login");
      const accountList = await loadAccounts();
      await refreshUsage(accountList);
      return account;
    } catch (err) {
      throw err;
    }
  }, [loadAccounts, refreshUsage]);
'@ -New @'
      const account = await invokeBackend<AccountInfo>("complete_login");
      await loadAccounts();
      return account;
    } catch (err) {
      throw err;
    }
  }, [loadAccounts]);
'@

Replace-Exact -Path $hook -Description 'Disable usage refresh after slim import' -Old @'
        const accountList = await loadAccounts();
        await refreshUsage(accountList);
        return summary;
      } catch (err) {
        throw err;
      }
    },
    [loadAccounts, refreshUsage]
  );

  const exportAccountsFullEncryptedFile
'@ -New @'
        await loadAccounts();
        return summary;
      } catch (err) {
        throw err;
      }
    },
    [loadAccounts]
  );

  const exportAccountsFullEncryptedFile
'@

Replace-Exact -Path $hook -Description 'Disable usage refresh after full encrypted import' -Old @'
        const accountList = await loadAccounts();
        await refreshUsage(accountList);
        return summary;
      } catch (err) {
        throw err;
      }
    },
    [loadAccounts, refreshUsage]
  );

  const cancelOAuthLogin
'@ -New @'
        await loadAccounts();
        return summary;
      } catch (err) {
        throw err;
      }
    },
    [loadAccounts]
  );

  const cancelOAuthLogin
'@

Replace-Exact -Path $hook -Description 'Disable startup and periodic usage refresh' -Old @'
  useEffect(() => {
    loadAccounts().then((accountList) => refreshUsage(accountList));
    
    // Auto-refresh usage every 60 seconds (same as official Codex CLI)
    const interval = setInterval(() => {
      refreshUsage().catch(() => {});
    }, 60000);
    
    return () => clearInterval(interval);
  }, [loadAccounts, refreshUsage]);
'@ -New @'
  useEffect(() => {
    void loadAccounts();
  }, [loadAccounts]);
'@

Replace-Exact -Path $app -Description 'Disable usage refresh after full backup import' -Old @'
      const accountList = await loadAccounts();
      await refreshUsage(accountList);
      const maskedIds = await loadMaskedAccountIds();
'@ -New @'
      await loadAccounts();
      const maskedIds = await loadMaskedAccountIds();
'@

Write-Host 'Manual-only usage refresh patch applied successfully.'
