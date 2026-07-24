# PHPStan との内部構造の相違

PHP;STEINS(`steins`)は模倣品なので当然多くの要素が PHPStan に依拠しているが、
*組織の陰謀* により止むをえず内部構造を大きく変えざるを得なかった要素がある。

以下は、その主要な相違点の一覧である。各項目は「PHPStan の構造 → 変更を
強いられた理由 → Steins の構造」の順で述べ、決定の典拠となる ADR を示す。
網羅的な登録簿は [ADR-0030 の divergence registry](adr/0030-type-semantics-phpstan-core-divergence-registry.md)
と [type-specification/divergence-registry.md](type-specification/divergence-registry.md)
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
のみで、厳格化は `default` → `throws-direct` → `contracts` という**名前付き
段階**へのオプトインである(lenient-default 原則、ADR-0050 amendment)。
数値レベルは refuse された — 段階には名前と定義があり、番号はない。

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
バージョン模倣行列は refuse。sidecar なしは「静かになる sound subset」で
あり、その沈黙は名指しされる。

## 楽観的 maybe 報告 vs zero-FP proof 層

PHPStan は「おそらく壊れる」を含めて幅広く報告し、benevolent union などの
補償機構で最悪ケース推論の副作用を和らげる。

Steins の proof 層は**確定 No のみ**を報告する(ADR-0002): 完全列挙の下で
のみ absence を主張し(ADR-0049 — dam・homonym・条件付き宣言・enum・
モンキーパッチ拡張まで沈黙脚が明文化される)、maybe は maybe のまま沈黙する。
補償機構は不要になったので存在しない。狼少年の撲滅が最優先原理であり、
held-out 実アプリ 14 本(約 23.7 万ファイル)で FP ゼロがその検収である
(notes/20260724-adoption-drill-record.md)。

## call-site テンプレートソルバー vs 透過テンプレート

PHPStan は呼び出し点でテンプレート型変数を単一化するソルバーを持つ。

Steins にソルバーはない(ADR-0032): 値伝播が届く範囲でテンプレートは
**透過**であり(`Box<int>` は `new` に流れ込んだ引数値を運ぶだけ)、
宣言レシーバ方向の解決だけを行う。届かない場所は沈黙する。受け入れた
コストは薄いライブラリ作者向け lint の不在で、それは登録簿に記録されている。

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
