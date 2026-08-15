# PHPStan との内部構造の相違

PHP;STEINS(`steins`)は模倣品なので当然多くの要素が PHPStan に依拠しているが、
*組織の陰謀* により止むをえず内部構造を大きく変えざるを得なかった要素がある。

以下は、その主要な相違点の一覧である。各項目は「PHPStan の構造 → 変更を
強いられた理由 → Steins の構造」の順で述べ、決定の典拠となる ADR を示す。
網羅的な登録簿は [ADR-0030 の divergence registry](../adr/0030-type-semantics-phpstan-core-divergence-registry.md)
と [type-specification/divergence-registry.md](../type-specification/divergence-registry.md)
にある。本書はその読み物版であり、両者が食い違う場合はレジストリが勝つ。

## Type hierarchy + TypeCombinator vs 四層値ドメイン + 構文的 arm リスト

PHPStan の型は `Type` インターフェースの豊かなクラス階層であり、
`TypeCombinator` が union/intersection の正規化代数を担う。`Type::equals` と
`isSuperTypeOf` が別々に存在し、accessory 型(non-empty-string 等)は
intersection として型に合成される。

Steins は値の側に真実を置いた。実行時に観測される値の集合を
Singleton / OneOf / Refined / General の四層ドメイン(ADR-0035)で持ち、
宣言型は正規化しない**構文的 arm リスト**のまま、単一の受理関係
(`admits_*`、trinary の Certainty)を通して arm ごとに判定する(ADR-0030)。
型結合代数は存在せず、結合は値ドメインの join が担う。型の「等しさ」は
相互包摂(Yes/Yes)としてのみ定義され、来歴フレーバー型
(`literal-string` 等)は等値判定の語彙から型システムのレベルで排除されている
(ADR-0030 registry entry 5)。TypeCombinator 相当の正規化器は、必要になった
時点で honesty renderer から**抽出**された(`steins_contract::normalize`、
ADR-0052 N1)— 先回りして構築しない、が規律である。

## Levels 0–9 vs 層(layer)+ 名前付き段階(profile)

PHPStan の厳格度は数値レベルの梯子で、レベル N で何が報告されるかは表を
引かないと分からない。

Steins は診断そのものに**意味論的な層**を持たせた: proof(実行時破壊の証明、
zero-FP)/ contract(宣言契約違反 = 負債報告)/ mechanics(装置自身の防錆)/
debug(要求された内省)(ADR-0050/0053)。既定の表示面は proof + mechanics
のみで、厳格化は `default` → `throws-direct` → `contracts` → `strict` という
**名前付き段階**へのオプトインである(lenient-default 原則、ADR-0050
amendment)。数値レベルは refuse された — 段階には名前と定義があり、番号はない。

なお PHPStan が「存在しないかもしれない offset の読み」を
`reportPossiblyNonexistentConstantArrayOffset` /
`…GeneralArrayOffset` という**設定フラグ**の裏に置くのに対し、Steins は
各診断 ID が梯子上の位置を示す `surface_floor` 属性をひとつ持ち
(ADR-0062 A-G10)、possibly 級の offset 診断は **measurement-first** で
出荷される: 有効化の前に triage 計器がプロジェクトを実測し、その実態を
見てから面を上げる。zero-FP とは「プロジェクトの実態に合わせた既定の校正」
であって「偽陽性になりうる検査の省略」ではない、という所有者ルーリング
(2026-07-29)がこの配線の典拠である。

## treatPhpDocTypesAsCertain vs 信頼の層序(stratum)

PHPStan は docblock 型を「確実として扱うか」をグローバルなトグルで切り替える。

Steins では信頼の順序が固定されている(ADR-0037): native 宣言と実行された
ガードは Verified、docblock 由来の主張は Asserted という**検査されるビット**
を事実そのものが運び(ADR-0052 N2)、導出(fold・配列合成・join)は常に
min-stratum を継承する。proof 層の診断は全前提 Verified を要求するため、
嘘の `@phpstan-assert` が証明を偽造することは構造的にできない。トグルは
存在しない — 設定が変えてよいのは報告面であって推論ではない。

## ignoreErrors(message 正規表現)vs 診断 ID レジストリ + baseline

PHPStan の抑制はエラーメッセージへの正規表現マッチが主力で、文言変更が
抑制を壊す。

Steins は診断 ID を(id, layer)のレジストリで管理し(ADR-0022)、抑制は
3 チャネルに限定される: インライン `@steins-ignore`(不一致は
`suppress.unmatched` として腐敗検知)、JSONL baseline(capture-surface
ヘッダ付き、面外エントリは dormant)、スコープ付きポリシー(ADR-0023)。
メッセージ文言は契約ではない。

## 多バージョン型解決 vs ask-the-real-thing(sidecar)

PHPStan は PHP バージョンをエミュレートし、シグネチャマップで複数バージョン
の組み込み関数を解決する。

Steins はプロジェクトが実際に動く PHP に**訊く**(ADR-0004/0024): 常駐
PHP sidecar が定数畳み込み・環境情報(バージョン・SAPI・拡張一覧)・存在
オラクル(`reflect`)を担い、組み込みの実在はカタログではなく boot surface
が答える(ADR-0049 §1 — カタログは不在の oracle には決してならない)。
sidecar なしは「静かになる sound subset」であり、その沈黙は名指しされる。
ランタイム優先はリリース優先の現姿勢であって最終形ではない:
**下位バージョンのシグネチャ差分**の扱い(library-range checking)は
今後の方向として意図されている — deferred であって refuse ではなく、
現段階では実装側の受け入れ準備も意図的にしていない。refuse のままなのは
「プロジェクトが実際には動かないバージョンの模倣」である。

## 楽観的 maybe 報告 vs zero-FP proof 層

PHPStan は「おそらく壊れる」を含めて幅広く報告し、benevolent union などの
補償機構で最悪ケース推論の副作用を和らげる。

Steins の proof 層は**確定 No のみ**を報告する(ADR-0002): 完全列挙の下で
のみ absence を主張し(ADR-0049 — dam・homonym・条件付き宣言・enum・
モンキーパッチ拡張まで沈黙脚が明文化される)、maybe は maybe のまま沈黙する。
補償機構は不要になったので存在しない。狼少年の撲滅が最優先原理であり、
held-out 実アプリ 14 本(約 23.7 万ファイル)で FP ゼロがその検収である
(../notes/20260724-adoption-drill-record.md)。

## call-site テンプレートソルバー vs 透過テンプレート

PHPStan は呼び出し点でテンプレート型変数を単一化するソルバーを持つ。

Steins にソルバーはない(ADR-0032): 値伝播が届く範囲でテンプレートは
**透過**であり(`Box<int>` は `new` に流れ込んだ引数値を運ぶだけ)、
宣言レシーバ方向の解決だけを行う。届かない場所は沈黙する。単一化の代わりに
Steins が行うのは**読み取り**である: 最上位の `@param Owner<…, T, …>` は
引数のジェネリクス carry に `T` の位置にあるものを尋ね、`@return T` はそれを
名指す。位置による一度の参照だけで、単一化も不動点もない。そして名前の
**すべての出現**に対して all-or-nothing である: 読み取れない位置
(`\Closure():T`、`list<Box<T>>`)は読み飛ばされるのではなく名前を係争中に
するので、答えが宣言の主張より狭くなることはない。常に本体サマリの下位に
位置する(サマリが語る場所では常にサマリが勝つ)。受け入れたコストは薄いライブラリ作者向け lint の
不在と、PHPStan なら解ける入れ子位置・境界付きテンプレートでの沈黙であり、
それは登録簿に記録されている。

## ImpurePoint vs Effect System

PHPStan は純粋性検査のために関数体の不純な箇所を `ImpurePoint` として列挙し、
`@phpstan-pure` の検証に使う。個々の「不純な点」の列挙であり、不純さの
**種類**は平坦である。

Steins はここを第二の推論次元に拡張した(ADR-0005/0018): 効果は
`io.filesystem.read` のような**階層的ドットパスラベル**の開かれたレジストリ
であり、prefix 包摂で束ねられ、関数は `#[\Steins\Effect]` /
`#[\Steins\Pure]` の**エンベロープ**(宣言された効果の上界)を持てる。
推論はエンベロープ超過(`effect.envelope-exceeded`)を via-provenance の
不動点で検出し、Liskov 拡大(`effect.liskov-widened`)も追う。つまり
ImpurePoint が「不純である点の証拠集め」であるのに対し、Effect System は
「副作用の型付け」である — 副作用のあるコードとテスト可能なコードを構造的に
分離するという、このプロジェクトの最終目的(consult-rector の後継としての
リファクタリング支援)がこの拡張を強いた。

条件付き純度の章も同じ形で分岐する(ADR-0063、批准待ち)。PHPStan で
維持者が合意した高階純度の解は `@pure-unless-callable-is-impure` という
**宣言**である — modular 解析はコールバックの本体を見られないからだ。
Steins は**意味論を先に**答える: 即時起動コールバック位置のカタログを引き、
可視のコールバックの envelope を既存の不動点で join する。宣言形を参照する
のは本体の見えない不透明 `callable` 引数だけである。by-ref out 引数
(`preg_match` の `$matches`)には Pure envelope が許容する `mutate.local`
色を与える — PHPStan 側で「嘘のフラグ」として二度却下された関数単位の
`hasSideEffects=false` は採らない。

ただし純粋性の docblock 表記は、分岐一辺倒ではない。ADR-0082 は
`@phpstan-impure <labels>` と `@phpstan-pure` を**相互運用エンベロープ**
として読む — 同じエンベロープ概念を、PHPStan の作者自身がタグの引数
位置として示したかたちで docblock に(未検査だが検査可能に)綴った
ものだ。`steins transform effects-envelope` はプロジェクト自身の証明
済みエフェクトからこれを書き戻すので、橋渡しは双方向に働く。裸の
`@phpstan-impure` は読まれないままだが(⊤ は情報を持たない)、パラ
メータ付きおよびクラスレベルの形式は宣言レーンに入り、宣言自身に対
して契約検査される。Steins のレジストリにないラベルは、タグ全体を
「未指定」として読む — 狭められた上界としてではない(オーナー裁定、
2026-08-12)。これにより、タイポは現行 PHPStan の下ですでにそうで
あるように静かに通り過ぎ、`#[\Steins\Effect]` の下で未知ラベルが実
行を失敗させるようには扱われない。詳細は
[phpdoc-effects-interop.md](../type-specification/phpdoc-effects-interop.md)
を参照。

## ConstantArrayType vs order-witnessed な値 + order-declared な shape

PHPStan の `ConstantArrayType` はひとつのクラスが宣言キー順・
`optionalKeys`・`nextAutoIndexes`・`isList` フラグを併せ持つ。そして
宣言順の信用が**一貫していない**: 受理は順序非依存なのに、位置射影
(`array_keys` / `array_values` / `array_slice` / `array_reverse`)は
宣言順を実行時順として読む — 到達可能な分岐を "always false" と誤報する
実在の FP クラスがここから生じる(#14940)。

Steins は真実を**来歴(provenance)**で分けた(ADR-0062): 値レーンは
**order-witnessed** — 挿入順を実際に観測した具体配列であり、順序依存の
結果が健全なのはここだけ。抽象側は単一の正準 **shape fact** — fields
(キー・presence(それ自身が信頼層序を持つ)・値スロット)+
sealed/unsealed tail + 外延的 `isList` 三値 + 非空性 + KeyCover — が
第五の fact 形であり、`array` / `array<K, V>` / `list<T>` / `array{…}` は
すべてこの一形の退化ケースである。具体配列を shape 世界へ持ち上げる瞬間が、
order-witnessed 性が正直に失われる場所として明文化されている。shape しか
知らない場面での位置射影は健全な widening のみを取り、宣言順は決して
読まない — ただし shape 自身の `isList` fact が `Yes` の場合は例外で、
これは**実現可能な順序**である: 許容されるすべての値がキー `0..n-1` を
その列で持つので、`array_values` / `array_keys` / `array_reverse` は
これを正確に消費する。順序を消費するのは意味論的保証(証明された列)で
ある場合だけであり、宣言物としての順序 — 上記 FP クラスが越えた一線 —
では決してない。#14939 のモデル(`array{…}` はキー**集合**、`list{…}` はキー**列**、
`isList` は許容値集合上で計算)は PHPStan stable に先行してネイティブに
走る — `list{…}` 受理の順列拒否と、**綴り**を含む。綴りはオラクルでは
なくモデルに従う: sealed shape の頭キーワードはその shape 自身の `isList`
であり、キー列だと証明できたときだけ `list{…}`、それ以外は `array{…}` —
出力を読み戻せば必ず同じ `isList` になる。PHPStan stable の
`ConstantArrayType` は両者を同一視しており(その `array{A, B}` は宣言順を
保持するので、我々の `list{A, B}` を意味する)、同一視しない代償は明示して
おく: nsrt の headline(`match`)は下がり、その分は `subsumed` — 我々が列を
主張しオラクルが集合を主張するときの正しい判定「表明より狭い」— に移る。
キーの**配置**は従来どおり PHPStan と同じ綴り(連続する必須キーは位置記法、
それ以外は全キーを明示、必須キーが含意する `non-empty-` は落とし、空 shape
は `array{}`)であり、unsealed shape は不変。
理由付きの
不採用: 抽象 `nextAutoIndexes`(具体側のみ・バージョン対応 A12)、
`ARRAY_COUNT_LIMIT` 型の union 縮退(計算された OneOf 降下で置換;256 は
単一 shape のフィールド幅上界としてのみ生存)。

## 式キーの narrowing vs cover fact + arm 減算

PHPStan の Scope は narrowing を式単位で持つため、
`isset($x['a']) || isset($x['b'])` という**選言の事実**を
`$x['a'] ?? $x['b']` の右腕まで運べない — 本作業の発端となった FP が
これである。Steins は shape fact 自身に **KeyCover** を記録する:
キー集合の反鎖で、`Isset`(非 null で存在する要素がある)と
`KeyExists`(存在するが null かもしれない)の二風味を持ち、`??` での
discharge 強度が本当に異なる(KeyExists cover は不在側スロットが非
nullable のときだけ discharge する — present-null は実行時にフォール
スルーするからで、これは不正確さではなく PHP の意味論そのものである)。
判別 union は arm レーンに住み、**減算**で絞られる: sealed が効かせる
isset 判別と、定数キー射影上の `match` / `===` によるタグ判別(フィールド
契約の `admits` が No の arm を消す)。1 本に収束した時点で shape fact が
鋳造される(ADR-0062 A-G3/A-G4/A-G8/A-G11)。

## DynamicReturnTypeExtension vs 五つの名前付き継ぎ目

PHPStan は呼び出しごとの戻り型計算とガード narrowing を実行時プラガブルな
拡張クラス群(`Dynamic*ReturnTypeExtension` / `*TypeSpecifyingExtension`)
として出荷する。Steins はこのための**拡張機構を作らない**(ADR-0064、
批准待ち): 輸入する各挙動は既存の五つの継ぎ目 — sidecar 畳み込み /
記号的な引数依存転送則 / probe でゲートされたキュレーション行 /
プラグイン面(フレームワーク魔術はここ)/ ガード語彙 — のちょうど
ひとつに分類され、輸入の優先度は conformance 表と corpus 頻度という
計測が決める。六つ目の開いたフックはプラグイン契約と競合する第二の
拡張機構になるため refuse である。
