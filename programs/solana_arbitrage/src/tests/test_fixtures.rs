use crate::InstructionData;

pub const PUBKEYS_LIST: &[(&str, bool)] = &[
    ("FYnaLRpfVbAi5CnupX1JuxqokiR773WiZPiCz3dzp7BP", true),  // payer [0] w
    ("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA", false),  // token_program [1] r
    ("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb", false),  // token_program_2022 [2] r
    ("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr", true),  // memo [3] w
    ("So11111111111111111111111111111111111111112", false),  // wsol_mint [4] r
    ("Ft6ingqkyR9JkdddhFUhTtKozr2ZbZssA9nu7sPLNtsk", true),  // user_wsol_ata [5] w
    ("6p6xgHyF7AeE6TZkSmFsko444wqoP15icUSqi2jfGiPN", false),  // mint [6] r
    ("96z2TkszvgVBQuLHeMfM9mgRmoV99FPbYftMSRHVdo6X", true),  // user_token_ata [7] w
    ("whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc", false),  // whirlpool program_id [8] r
    ("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C", false),  // raydium_cpmm program_id [9] r
    ("GpMZbSM2GgvTKHJirzeGfMFoaZ8UR2X7F4v8vHTvxFbL", false),  // raydium_cpmm vault_authority [10] r
    ("Ckp1kwZqosaLU1h3zWtuaMBubyWM7LX3cxYezRVin7p2", true),  // whirlpool [Ckp1kwZq] pool_id [11] w
    ("9noUm1EgtqC5qD6ERzZhW7CnM4xNav2yzDkHjVXGyXGJ", true),  // whirlpool [Ckp1kwZq] base_vault [12] w
    ("DUr3cQ1VQZVuakqo7oFD9ZGu5h1QYnqdcrFpXJwnBVgw", true),  // whirlpool [Ckp1kwZq] quote_vault [13] w
    ("5tNrhqbdXgRJEGLYKmjhEUFbfN3FmzVkexY2hawcKLVW", true),  // whirlpool [Ckp1kwZq] oracle [14] w
    ("EtVU9dJFd93shSCqmiBEEtWW6x9oNFuVT3VpDm5EdN9n", true),  // whirlpool [Ckp1kwZq] tick_array_0 [15] w
    ("6fjCurxSoA7Edjfmbr9e2rFkLX7eZ3mzfWWdAWb2BAUp", true),  // whirlpool [Ckp1kwZq] tick_array_1 [16] w
    ("FUxPVUyMytvwyngVWqjtU9t8UvbTBaRUayzi586HCET9", true),  // whirlpool [Ckp1kwZq] tick_array_2 [17] w
    ("6KX9iiLFBcwfjq3uMqeeMukaMZt5rQYTsbZZTnxbzsz6", true),  // whirlpool [6KX9iiLF] pool_id [18] w
    ("FeFgcWCxBx15ESeh5ahaXi4jujxk2HrQD1RqaRqYxgpf", true),  // whirlpool [6KX9iiLF] base_vault [19] w
    ("79mhWQ3ppuBeVNikPoazsCorSiUZEAg3H7dgkdqzVU8x", true),  // whirlpool [6KX9iiLF] quote_vault [20] w
    ("8uxqxLsg3iDkWLkhp6m1Kbgw53LVQDZU4U17i8Zmxg1d", true),  // whirlpool [6KX9iiLF] oracle [21] w
    ("BuwDXXpgU57p89E9ttS2ksJkVkCGwWNxcxtmRL8pWxuK", true),  // whirlpool [6KX9iiLF] tick_array_0 [22] w
    ("2a12D3csaGyhSSHV2Skju8iwAv7MAARkzEJjEpGTnK1s", true),  // whirlpool [6KX9iiLF] tick_array_1 [23] w
    ("5U4jU9nbX9sAeWg1KrC39hi63BnPRxJvXJhjMENWsrhG", true),  // whirlpool [6KX9iiLF] tick_array_2 [24] w
    ("HKuJrP5tYQLbEUdjKwjgnHs2957QKjR2iWhJKTtMa1xs", true),  // raydium_cpmm [HKuJrP5t] pool_id [25] w
    ("7wMM5Tg7igkefH1T2TKqJBpYp5bQKPQjz7yTgvCUZY6Z", true),  // raydium_cpmm [HKuJrP5t] base_vault [26] w
    ("Gy2JYhV9gAZUBrjq35St78VMrXiufU72Que26pmhMYob", true),  // raydium_cpmm [HKuJrP5t] quote_vault [27] w
    ("D4FPEruKEHrG5TenZ2mpDGEfu1iUvTiqBxvpU8HLBvC2", false),  // raydium_cpmm [HKuJrP5t] amm_config [28] r
    ("HSYeHzVCyb2GmVqhug9jP2BgZj5jswgrpU5P8GtfA5M3", true),  // raydium_cpmm [HKuJrP5t] observation [29] w
];

pub fn make_instruction_data(test_mode: bool) -> InstructionData {
    InstructionData {
        mints: 2,
        shared_statics_len: 3,
        pool_types: [4, 4, 7, 0, 0, 0, 0, 0],
        type_static_offsets: [0, 0, 1, 0, 0, 0, 0, 0],
        mode: 0,
        test: false,
        group_sizes: [0, 0, 0, 0],
        pool_fees: vec![],
    }
}
