package example

import java.time.Instant
import scala.collection.mutable.ListBuffer
import scala.concurrent.duration.*
import scala.util.Try

object Main:
  val timeout: FiniteDuration = 1.second
  val now: Try[Instant] = Try(Instant.now())
  val buffer: ListBuffer[Int] = ListBuffer(1)
