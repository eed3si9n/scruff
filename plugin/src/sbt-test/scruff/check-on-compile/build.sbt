ThisBuild / scalaVersion := "3.8.4"
ThisBuild / scruffBinary := file(sys.props("scruff.binary"))
ThisBuild / scruffFmtOnCompile := true
ThisBuild / scruffCheckOnCompile := true

lazy val root = project in file(".")

lazy val off = project.settings(scruffCheckOnCompile := false)
