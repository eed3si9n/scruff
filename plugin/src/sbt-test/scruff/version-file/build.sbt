ThisBuild / scalaVersion := "3.8.4"

val checkVersion = inputKey[Unit]("Fails unless scruffVersion is the given version")
checkVersion := {
  val want = Def.spaceDelimited().parsed.head
  val got = (ThisBuild / scruffVersion).value
  assert(got == want, s"scruffVersion is $got, not $want")
}
