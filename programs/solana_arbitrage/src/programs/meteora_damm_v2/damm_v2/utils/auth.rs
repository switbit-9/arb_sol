use pinocchio::pubkey::Pubkey;

#[cfg(not(feature = "devnet"))]
pub mod admin {
    use pinocchio::pubkey::Pubkey;

    pub const ADMINS: [Pubkey; 2] = [
        five8_const::decode_32_const("5unTfT2kssBuNvHPY6LbJfJpLqEcdMxGYLWHwShaeTLi"),
        five8_const::decode_32_const("DHLXnJdACTY83yKwnUkeoDjqi4QBbsYGa1v8tJL76ViX"),
    ];
}

#[cfg(feature = "devnet")]
pub mod admin {
    use pinocchio::pubkey::Pubkey;

    pub const ADMINS: [Pubkey; 3] = [
        five8_const::decode_32_const("5unTfT2kssBuNvHPY6LbJfJpLqEcdMxGYLWHwShaeTLi"),
        five8_const::decode_32_const("DHLXnJdACTY83yKwnUkeoDjqi4QBbsYGa1v8tJL76ViX"),
        five8_const::decode_32_const("4JTYKJAyS7eAXQRSxvMbmqgf6ajf3LR9JrAXpVEcww2q"), // minh
    ];
}

#[cfg(feature = "local")]
pub fn assert_eq_admin(_admin: Pubkey) -> bool {
    true
}

#[cfg(not(feature = "local"))]
pub fn assert_eq_admin(admin: Pubkey) -> bool {
    admin::ADMINS
        .iter()
        .any(|predefined_admin| predefined_admin.eq(&admin))
}
