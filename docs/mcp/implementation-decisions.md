# Contrats d'implémentation MCP local V1

Décisions du 5 novembre 2026, lot 0. Windows x86_64 MSVC seulement ; application ouverte, éventuellement dans le tray ; une installation active par compte Windows. Profils arbitraires : « perso » et « work » sont des fixtures.

## SDK et protocole

- SDK officiel publié **rmcp = 3.8.0**, Apache-2.0, MSRV 1.88 ; toolchain de validation Rust 1.93.1.
- Serveur : `default-features = false`, features `server`, `macros`, `transport-async-rw`. `server` entraîne schemars/uuid et le transport ; aucun HTTP/OAuth/TLS nécessaire.
- La liaison autonome relaie les octets via Tokio après authentification ; aucune dépendance rmcp/Tauri/audio/Whisper ni accès aux fichiers métier.
- `supported_protocol_versions()` est limité à **2026-07-28** et **2025-11-25**, pas toutes les versions connues par défaut.
- Version actuelle : `server/discover`, puis chaque appel porte `io.modelcontextprotocol/protocolVersion`, `io.modelcontextprotocol/clientInfo` et `io.modelcontextprotocol/clientCapabilities` dans `params._meta`.
- Version 2025-11-25 : `initialize`, `notifications/initialized`, appels historiques. Utiliser les types/cycle de vie SDK, sans réimplémenter initialize.
- Le spike utilise un **outil ping → pong**, pas la méthode historique MCP ping (absente en 2026-07-28). V1 ne dépend pas de sampling, elicitation ou subscriptions.
- Transport produit : `AsyncRwTransport::new(read, write).with_max_line_length(8_388_608)` ; défaut SDK 16 Mio. Borner aussi le relais avant buffering.

Le spike jetable est `.superpowers/sdd/2026-11-05-local-mcp/spike/`, avec Cargo.lock et target isolés. Il utilise uniquement du contenu synthétique et ne contient pas l'authentification, les grants ou la DACL SID de production ; ces exigences restent bloquantes au lot 5. Résultats exécutés : voir le rapport de lot 0.

Références : [SDK 3.8.0](https://docs.rs/rmcp/3.8.0/rmcp/), sources publiées handler/server.rs, service/server.rs, transport/async_rw.rs et tests/test_server_discover.rs.

## IPC privé v1

Le handshake est invisible au client stdio. Après Accepted, framing MCP standard JSON-RPC UTF-8 terminé par LF.

- Pipe `\\.\pipe\lexena-mcp-<installation_uuid>`. Descripteur privé `mcp/endpoint.json` : `installation_id`, `pipe_name`, `server_path` canonique, `version: 1`, `app_version`. Écriture atomique, aucun secret.
- Frame : longueur **u32 little-endian**, JSON UTF-8 de **1 à 4096 octets**, champs inconnus refusés, délai total **5 s**, aucun downgrade implicite.
- Ordre : `ClientHello { version:1, role:"client", installation_id, connection_id, nonce }` → `ServerHello { version:1, role:"server", nonce, proof }` → `ClientProof { role:"client", proof }` → `Accepted { accepted:true }`. Toute erreur ferme le flux avant MCP.
- Nonces et preuves : tableaux JSON de exactement 32 octets entiers (0..255), tailles contrôlées par les types serde. Transcript : ASCII `lexena-mcp-ipc-v1\0`, rôle ASCII `server\0` ou `client\0`, UUID installation 16 octets RFC, UUID connexion 16 octets RFC, nonce client 32 octets, nonce serveur 32 octets. Le domaine lie la version.
- HMAC-SHA256, secret de connexion 256 bits dans keyring Windows existant, nonces CSPRNG OS. Comparaison constante des 32 octets ; vérifier preuve serveur avant de fournir preuve client. Échec fermé sans coffre, aucune sauvegarde de secret.
- DACL explicite privée SID courant, non héritée ; reject_remote_clients(true) ; first_pipe_instance(true). Vérifier côté liaison GetNamedPipeServerProcessId, QueryFullProcessImageNameW, chemin canonique attendu et cohérence avec l'installation du bridge. Aucun scan d'autres pipes.
- Maximum 8 connexions, 4 requêtes métier en vol/connexion, délai ordinaire 30 s. Révocation ferme les sessions et bloque les nouveaux commits ; terminer les commits déjà durables.
- `mcp-ipc` : framing/auth/endpoint/pipe uniquement, sans Tauri ni métier. Pas de bind TCP/UDP/HTTP. Refus explicite des plateformes non validées.

La protection ne constitue pas une sandbox contre un logiciel malveillant sous le même compte Windows.

Les structures ci-dessus décrivent les trames privées effectivement testées, avec rôles explicites et tableaux d'octets de taille fixe. Elles remplacent les noms illustratifs et l'encodage hex envisagés au spike. Ce canal interne est commun aux deux binaires livrés ; les clients IA ne le parlent pas et utilisent le framing MCP stdio standard.

## Stockage et réconciliation livrés

Le HTML et les métadonnées restent dans le format de notes existant. `profiles/<id>/note-service/` contient le journal de redo, les révisions, reçus, versions et intentions locales. Toutes les mutations UI, imports et MCP passent par le service commun ; les éditeurs et la queue utilisent une session de profil capturée.

`sync-queue.json` a un propriétaire Rust unique avec snapshot atomique, CAS, DLQ et watermark d'import. Les marqueurs de protection sans contenu conservent localement les éléments temporairement inéligibles. Une intention acquittée dans la queue n'est pas une confirmation cloud. L'ordre confirmé reste profil, dossiers, puis notes.

Le cache de lecture dérivé est borné à 32 Mio. Il réutilise les révisions par note, se renouvelle lors des mutations du service et ne constitue pas un nouveau stockage de référence. Une construction longue ne retient pas le verrou d'autorisation : une vérification actuelle précède la construction et une nouvelle autorisation retenue précède tout résultat. Les mutations durables gardent leur autorisation pendant tout le commit. Un lecteur public ne charge pas le corps d'une note locale pour construire son cache.

## Dépendances publiées et verrouillées

| Usage | Crate exacte | Licence | MSRV déclaré |
| --- | --- | --- | --- |
| DOM HTML5/sélecteurs | `scraper =0.29.0`, default-features=false | ISC | non déclaré, edition 2024 |
| Markdown vers HTML | `pulldown-cmark =0.13.6`, default-features=false, features=[html] | MIT | 1.71.1 |
| HTML vers Markdown de lecture | `htmd =0.5.6` | Apache-2.0 | non déclaré, edition 2024 |
| CSPRNG OS | `getrandom =0.4.3` | MIT OR Apache-2.0 | 1.85 |
| HMAC | `hmac =0.13.0` | MIT OR Apache-2.0 | 1.85 |
| SHA-256 | `sha2 =0.11.1` | MIT OR Apache-2.0 | 1.88 |
| Comparaison constante | `subtle =2.6.1` | BSD-3-Clause | non déclaré |
| Remplacement Windows | `atomicwrites =0.4.4` | MIT | non déclaré |

Les MSRV non publiés ne sont pas inventés ; la validation V1 se fait sur **Rust 1.93.1**. Cargo.lock fige les transitives ; aucune dépendance Git flottante. Dépôts/licences déclarés vérifiés par cargo info et sources publiées. Pas de primitive crypto, parseur HTML ou remplacement Windows maison.

`scraper` expose un arbre HTML5 mutable et sérialisable. Validation DOM avec refus d'élément/attribut inconnu, aucune regex ni suppression silencieuse. Append conserve la chaîne HTML existante et ajoute les nouveaux blocs validés. Édition ciblée : occurrence exacte unique dans un nœud texte ; sérialisation préserve attributs et sémantique, pas nécessairement l'ordre des attributs/espaces d'origine.

Corpus basé sur les extensions réelles de useNotesEditorInstance.ts : titres, paragraphes, gras/italique/souligné/barré, span color, mark color, liens externes, `a[data-note-link][data-note-id][data-note-title]`, tableaux colspan/rowspan/colwidth, listes de tâches data-type/data-checked, pre/code, PNG data-URI inline, FR/EN/emoji/CJK. Le spike vérifie présence des attributs, texte Unicode et conversion dans les deux sens.

L'export Markdown est **toujours déclaré avec pertes** : styles, attributs, images et liens internes ne permettent pas un remplacement riche. Les lectures remplacent les images inline par des marqueurs bornés. Markdown entrant : refuser raw HTML et URLs dangereuses, puis valider le DOM résultant. Le corpus de faisabilité ne remplace pas les tests de sécurité du lot 4.

`atomic-write-file 0.3.1` a été évalué puis **écarté** : voie Windows générique fs::rename et TODO Windows dans imp/mod.rs. `atomicwrites 0.4.4` écrit dans un temporaire du même volume, sync_all avant publication, puis MoveFileExW(MOVEFILE_WRITE_THROUGH | MOVEFILE_REPLACE_EXISTING) ([source Windows](https://docs.rs/atomicwrites/0.4.4/src/atomicwrites/lib.rs.html)). Répertoire/temporaire privés par ACL. Atomicité par fichier seulement : journal/reprise multi-entités indispensables au lot 2. Une erreur de partage/antivirus refuse le commit, sans écriture directe de secours. Ne pas promettre une résistance à un stockage qui ment sur les flush matériels.

## Fixtures contractuelles communes

Aucune donnée réelle. IDs locaux du manifeste ; les noms ne servent jamais à construire un chemin.

| Fixture | ID synthétique | Propriété |
| --- | --- | --- |
| perso | fixture-perso | Actif UI, grant perso seulement |
| work | fixture-work | Inactif UI, grant work seulement |
| Création 研究 | fixture-third | Créé après grant, non accordé implicitement |
| Projet perso | 00000000-0000-4000-8000-000000000001 | Même titre que work, contenu distinct |
| Projet work | 00000000-0000-4000-8000-000000000002 | Append work ne modifie pas perso |
| Note riche work | 00000000-0000-4000-8000-000000000003 | Corpus HTML, lien interne et dossier work |
| Note locale | 00000000-0000-4000-8000-000000000004 | local_only=true, grant distinct requis |
| Note supprimée | 00000000-0000-4000-8000-000000000005 | Tombstone/snapshot ; absente des listes actives |
| Dossier work | 00000000-0000-4000-8000-000000000006 | Refus pour note perso |

Ajouter même UUID de note dans deux profils pour la course de sauvegarde après switch, et tailles UTF-8 aux frontières 32 768 / 3 145 728 / 8 388 608 octets. Aucun wildcard grant futur.

## Packaging et mise à jour

État vérifié : scripts/release.ps1 compile avec **pnpm tauri build**, localise lexena.exe et le NSIS signé, publie les deux artefacts/checksums/manifestes updater. release.yml génère les manifestes et scanne les artefacts **sans compiler**. tauri.conf.json cible NSIS uniquement. Ne pas placer le build de liaison seulement dans GitHub Actions.

Contrat lot 9 :

1. Crate autonome lexena-mcp, même version que l'application, target x86_64-pc-windows-msvc ; stage `src-tauri/binaries/lexena-mcp-x86_64-pc-windows-msvc.exe` ; `bundle.externalBin = ["binaries/lexena-mcp"]` ([convention Tauri](https://v2.tauri.app/develop/sidecar/)).
2. Hook avant build prépare la liaison avant bundling, respecte le target explicite. Aucun Node/Python/Cargo chez l'utilisateur.
3. Nom installé **lexena-mcp.exe**, voisin de **lexena.exe** dans l'installation NSIS. UI génère un chemin absolu dérivé de l'exécutable courant, vérifie sa présence. Confirmer par installation réelle au lot 9, pas seulement les docs.
4. Garder `lexena-v<VERSION>-portable.exe`. Ajouter `lexena-v<VERSION>-portable-mcp.zip` : lexena.exe, lexena-mcp.exe, mcp-version.json (schema 1, app/bridge même version, ipc 1, target et SHA-256 des EXE). Inclure ZIP dans checksums/scans. L'EXE seul ne promet pas MCP.
5. Refuser IPC/manifeste incohérent ; configuration `--connection <uuid>` sans secret. Pas de copie automatique des autorisations sur un autre PC ou une autre installation.
6. Updater NSIS existant, mêmes clés. Maintenance MCP, finir commits, fermer liaisons avant remplacement des EXE ; demander fermeture client si verrou Windows persistant. Mise à jour des deux binaires ensemble.

Le script de release et aucun build de production ne sont exécutés au lot 0. Installation réelle/PC propre, refus autre compte/pipe usurpé, révocation et interruption update restent les critères des lots 5/9. Aucune compatibilité macOS/Linux/ARM64 annoncée.

## Résultat exécuté du spike

Le 5 novembre 2026 : compilation Rust 1.93.1 réussie, deux liaisons stdio simultanées vers le même named pipe, discovery 2026-07-28 et initialize 2025-11-25 réussis, outil ping renvoyant pong dans les deux cas. Tous les stdout clients sont des lignes JSON-RPC ; stdout serveur vide. netstat ne montre aucun endpoint TCP/UDP pour les trois processus. Corpus DOM/Markdown/crypto/remplacement Windows et comparaison des documents TipTap avant/après sérialisation Rust réussis. Transcript synthétique complet et commandes dans le rapport de lot 0.

Extrait actuel : `tools/call` avec `name: ping`, `arguments: {}` et les métadonnées 2026-07-28 reçoit `{"jsonrpc":"2.0","id":3,"result":{"resultType":"complete","content":[{"type":"text","text":"pong"}],"isError":false}}`. Le même appel après initialize historique reçoit pong sans resultType récent.

Licences transitives vérifiées : cssparser, cssparser-macros, dtoa-short et selectors (via scraper) sont MPL-2.0 ; conserver les sources/notices de ces composants distribués. Les autres licences relevées sont MIT/Apache/ISC/BSD/Unicode/Unlicense/BSL au choix déclaré. Le package jetable seul n’a pas de licence déclarée ; aucune dépendance publiée du graphe n’est sans licence dans les métadonnées.
