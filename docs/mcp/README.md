# MCP local de Lexena

L'intégration est en développement sur la branche MCP. Les vérifications natives restant à effectuer sont listées dans [la checklist](e2e-checklist.md). Ne pas considérer cette documentation comme une annonce de disponibilité dans la version publiée.

La fonction et sa liaison seront fournies avec Lexena. « stdio » désigne un mode de communication avec le client IA, pas un logiciel supplémentaire à installer. L'accès est local au PC ; Lexena doit fonctionner, éventuellement masqué dans le tray. Les profils sont découverts dynamiquement, sans nom ni nombre imposé. Chaque connexion accède uniquement aux profils et permissions choisis localement.

## Connecter une IA

La V1 vise Windows x86_64. L'installateur inclut `lexena-mcp.exe` à côté de `lexena.exe`. Les utilisateurs n'ont besoin ni de Node, ni de Python, ni de Rust. Pour une distribution portable, extraire ensemble les deux exécutables et `mcp-version.json` du ZIP `portable-mcp`. L'ancien téléchargement EXE portable seul ne fournit pas cette intégration.

1. Ouvrir Lexena, puis **Réglages → Intégrations → MCP**.
2. Activer le MCP local, désactivé par défaut.
3. Créer une connexion nommée pour le client IA, sélectionner les profils autorisés et les permissions. La lecture, l'écriture, la suppression, le remplacement intégral et la lecture des notes locales ont des droits distincts.
4. Copier la configuration générée dans un client prenant en charge les serveurs MCP stdio locaux. Elle contient le chemin absolu de la liaison et `--connection <uuid>`, sans secret. Les capacités et la procédure d'ajout dépendent du client choisi.
5. Tester la connexion depuis Lexena, puis demander à l'IA de lister les profils accessibles.

Le protocole MCP est négocié par le SDK : `2026-07-28` et `2025-11-25` sont pris en charge. Aucun port TCP ou serveur HTTP n'est ouvert par cette intégration. Le client IA peut lui-même utiliser un modèle distant : l'accès local au logiciel ne garantit pas que le texte fourni à ce client reste sur le PC.

L'IA doit demander le profil lorsque la conversation ne permet pas de le déterminer. Les outils exigent toujours un `profile_id` explicite et autorisé ; ils ne prennent jamais le profil affiché comme valeur de repli. Cette obligation empêche un accès implicite, mais le serveur ne peut pas savoir si une IA a deviné un ID valide : les instructions et le comportement du client restent nécessaires pour la clarification.

## Périmètre et modifications

Le catalogue comprend 17 outils : contexte et profils ; liste des dossiers ; liste, recherche et lecture des notes ; création, ajout, édition ciblée, métadonnées, remplacement intégral, suppression et restauration ; versions ; création, modification et suppression des dossiers. La V1 ne pilote pas l'enregistrement audio, les comptes, les clés API ou les mises à jour. Les services communs permettent d'étendre ce périmètre ultérieurement.

Chaque mutation porte un `operation_id` UUID ; réutiliser le même ID et les mêmes arguments après une réponse perdue retrouve le reçu initial pendant 30 jours. Une modification existante exige sa révision actuelle. Un éditeur modifié ou indisponible provoque un conflit au lieu d'écraser son contenu. Les fenêtres propres sont mises en pause pendant le commit, puis actualisées.

Les versions créées avant les modifications MCP sont conservées 30 jours, avec un plafond local de 256 Mio par profil. Le plafond atteint refuse une nouvelle modification nécessitant une sauvegarde ; il ne supprime pas silencieusement une version encore protégée. Supprimer un dossier remet ses notes à la racine. Restaurer une note conserve sa préférence locale de confidentialité ; une préférence inconnue après purge reste locale.

La confirmation d'un outil décrit d'abord l'écriture locale. Son état de sync (`disabled`, `local_only`, `too_large`, `pending`, `deferred_profile_inactive`) n'est pas une confirmation de réception cloud. Le MCP n'active pas la synchronisation : les changements suivent l'outbox du profil concerné et la sync déjà configurée dans Lexena.

La recherche utilise un cache local dérivé, limité à 32 Mio et actualisé par les révisions du service. Sur le corpus de référence de 5 000 petites notes, la première construction a dépassé le délai de 30 secondes ; elle s'est terminée en arrière-plan et la demande peut être relancée. Les lectures suivantes mesurées sont détaillées dans [les résultats de validation](e2e-checklist.md). Une édition directe des fichiers hors de Lexena ne constitue pas une mutation prise en charge par ce cache.

## Révoquer et diagnostiquer

Les connexions peuvent être testées, révoquées ou renouvelées dans les réglages. Les permissions sont vérifiées à chaque appel ; une révocation attend qu'un commit déjà autorisé se termine et empêche les suivants. Les secrets restent dans le coffre Windows de ce poste. L'audit local contient des identifiants, outils, résultats et durées, sans texte des notes, titres, recherches ni secrets.

Si Lexena est fermé, ouvrir l'application puis reconnecter le client IA. Si les versions de Lexena et de la liaison diffèrent, réinstaller ou réextraire le paquet complet correspondant. Les empreintes du manifeste portable vérifient la cohérence des fichiers ; la vérification cryptographique de l'installateur reste celle du système de mise à jour existant.

Une installation déplacée ou des métadonnées MCP restaurées sur un autre poste peuvent demander une réinitialisation depuis les réglages. Cette action recrée l'identité MCP et ses autorisations, sans supprimer les notes. Avant une mise à jour, Lexena passe le MCP en maintenance, termine les commits et ferme les liaisons. Si un client garde l'exécutable ouvert, le diagnostic demande de fermer cette connexion puis de réessayer.

## Contrat de contenu des notes

- Création/ajout : texte brut ou Markdown (paragraphes, titres, listes, citation, code, tableaux et texte barré). Le HTML brut dans le Markdown est refusé. Les autres extensions Markdown ne sont pas promises par ce sous-ensemble.
- Le texte brut est échappé puis converti en paragraphes. L'ajout conserve les octets du HTML précédent et ajoute seulement les blocs produits.
- Édition ciblée : `old_text` doit correspondre exactement à une seule occurrence dans un seul nœud texte. Une absence, un chevauchement, plusieurs occurrences ou une correspondance entre nœuds sont refusés. Les chips de liens internes sont des nœuds atomiques et ne se renomment pas par remplacement textuel.
- Remplacement intégral : HTML complet compatible TipTap, permission distincte et lecture HTML complète de la révision concernée. Pas de reconstruction à partir d'une vue Markdown avec pertes.
- HTML accepté : paragraphes/titres, gras/italique/barré/souligné, citation, listes et tâches TipTap, tableaux et dimensions numériques, code/blocs de code, spans et couleurs contrôlées, liens internes avec leurs attributs, images raster inline PNG/JPEG/GIF/WebP en data-URI.
- Scripts, handlers, iframes/objets, SVG actif, styles non pris en charge, fichiers locaux et liens HTTP vers hôtes locaux/privés sont refusés. Aucun fetch d'URL n'est effectué par le serveur. Les hôtes sont vérifiés syntaxiquement ; la lecture d'une note n'effectue aucune résolution DNS et ne certifie pas le routage futur d'un lien public par le client IA.
- Un élément non pris en charge produit une erreur ; aucun nettoyage avec perte silencieuse du document.
- Lecture : texte par défaut, Markdown sur demande avec avertissement de perte, ou HTML stocké. Les vues texte/Markdown remplacent les images inline par `[image]` plutôt que retourner leurs octets.
- L'accès à une note autorise la lecture du texte et des attributs qu'elle contient déjà, y compris les libellés/IDs des liens internes. Le MCP ne suit pas ces liens et ne consulte pas la note cible sans ses permissions propres. Un texte privé cité dans une note accessible appartient donc au contenu transmis de cette source.
- Limite de mutation : 3 145 728 octets UTF-8 sur le HTML produit. Une note locale plus grande reste lisible par segments.
- Le titre suit la première ligne visible, avec la même règle dans l'éditeur et le MCP ; la limite de 50 unités UTF-16 ne coupe pas un caractère Unicode.

Les IDs nouveaux sont des UUID. Les IDs hérités sûrs restent accessibles sans renumérotation : composant unique borné, sans chemin, ADS, alias Windows ou reparse point. L'ID de profil est toujours explicite et autorisé.

## Nouveau PC

Installer Lexena avec sa liaison incluse, récupérer les profils effectivement synchronisés ou une sauvegarde locale compatible, puis créer l'autorisation d'accès IA sur le nouveau poste. Les notes locales, l'historique de transcription et l'audio ne sont pas transférés par la sync actuelle. Ne pas effacer la copie de l'ancien PC avant vérification de la récupération.

Les chemins et secrets d'accès IA sont propres au poste et ne sont pas copiés automatiquement. Une installation déplacée ou restaurée doit proposer la reconnexion dans l'interface locale, sans exiger de supprimer manuellement des fichiers.

## Désactivation et retour arrière

Désactiver le MCP ferme son écoute locale et conserve les notes, versions et intentions de synchronisation. Le service persiste ses révisions, reçus et journal dans le répertoire privé `profiles/<id>/note-service/`. Un format inconnu ou un journal non récupérable ferme l'accès au profil concerné avec un diagnostic ; il n'est pas remplacé par un état vide.

Les anciennes versions de Lexena antérieures à ce garde ne savent pas reconnaître ce format. Un retour à ces versions nécessite une sauvegarde compatible prise avant la migration, application fermée. Conserver les fichiers d'outbox et le journal pour diagnostiquer une interruption plutôt que les effacer.

La fonction reste volontaire et désactivée par défaut. La validation des clients IA et des installations natives est suivie dans [la checklist de livraison](e2e-checklist.md).
