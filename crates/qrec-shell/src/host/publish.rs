//! Socket publishing under the lock (A§8.3 step 1): unlink `<hash>.sock`,
//! bind `<hash>.sock.<pid>`, listen, `rename` it to `<hash>.sock`. Only the
//! lock holder unlinks its socket; scanners never delete anything.
//! Owner: store. Empty stub.
