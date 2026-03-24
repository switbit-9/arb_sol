pub mod alpha_vault {
    use pinocchio::pubkey::Pubkey;

    #[cfg(not(feature = "local"))]
    pub const ID: Pubkey =
        five8_const::decode_32_const("vaU6kP7iNEGkbmPkLmZfGwiGxd4Mob24QQCie5R9kd2");

    #[cfg(feature = "local")]
    pub const ID: Pubkey =
        five8_const::decode_32_const("SNPmGgnywBvvrAKMLundzG6StojyHTHDLu7T4sdhP4k");

    pub fn derive_vault_pubkey(vault_base: Pubkey, pool: Pubkey) -> Pubkey {
        let (vault_pk, _) =
            pinocchio::pubkey::find_program_address(&[b"vault", &vault_base, &pool], &ID);
        vault_pk
    }
}
