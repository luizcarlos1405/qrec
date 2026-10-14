//! GPU safety (A§8.10): resolve the effective VA render node (A§2.1; never
//! NVIDIA automatically) and set `LIBVA_DRIVER_NAME` and friends at the top
//! of `main()`, before any thread starts. Owner: capture. Empty stub.
